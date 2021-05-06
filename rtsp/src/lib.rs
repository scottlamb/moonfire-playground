use bytes::{Buf, BufMut, Bytes, BytesMut};
use failure::{Error, bail};
use once_cell::sync::Lazy;
use rtsp_types::Message;
use std::{convert::TryFrom, fmt::{Debug, Display}};

pub mod client;
pub mod mp4;

pub static X_ACCEPT_DYNAMIC_RATE: Lazy<rtsp_types::HeaderName> = Lazy::new(
    || rtsp_types::HeaderName::from_static_str("x-Accept-Dynamic-Rate").expect("is ascii")
);
pub static X_DYNAMIC_RATE: Lazy<rtsp_types::HeaderName> = Lazy::new(
    || rtsp_types::HeaderName::from_static_str("x-Dynamic-Rate").expect("is ascii")
);

#[derive(Debug)]
pub struct ReceivedMessage {
    pub ctx: Context,
    pub msg: Message<Bytes>,
}

const MAX_TS_JUMP_SECS: u32 = 10;

pub struct Timeline {
    latest: Timestamp,
    max_jump: u32,
}

/// A RTP/RTSP timestamp.
/// The [Display] and [Debug] implementations display:
/// *   the bottom 32 bits, as seen in RTP packet headers. This advances at a
///     codec-specified clock rate.
/// *   the full timestamp, with top bits accumulated as RTP packet timestamps wrap around.
/// *   a conversion to RTSP "normal play time" (NPT): zero-based and normalized to seconds.
#[derive(Copy, Clone)]
pub struct Timestamp {
    /// The full timestamp, with top bits inferred from RTP timestamp wraparounds.
    timestamp: u64,

    /// The codec-specified clock rate.
    clock_rate: u32,

    /// The stream's starting time, as specified in the RTSP `RTP-Info` header.
    start: u32,
}

#[derive(Copy, Clone, PartialEq, PartialOrd, Eq, Ord)]
pub struct NtpTimestamp(u64);

impl std::fmt::Display for NtpTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sec_since_epoch = ((self.0 >> 32) as u32).wrapping_sub(2_208_988_800);
        let tm = time::at(time::Timespec {
            sec: i64::from(sec_since_epoch),
            nsec: 0,
        });
        let ms = (self.0 & 0xFFFF_FFFF) * 1_000 >> 32;
        let zone_minutes = tm.tm_utcoff.abs() / 60;
        write!(
            f,
            "{}.{:03}{}{:02}:{:02}",
            tm.strftime("%FT%T").or_else(|_| Err(std::fmt::Error))?,
            ms,
            if tm.tm_utcoff > 0 { '+' } else { '-' },
            zone_minutes / 60,
            zone_minutes % 60
        )
    }
}

impl std::fmt::Debug for NtpTimestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Write both the raw and display forms.
        write!(f, "{} /* {} */", self.0, self)
    }
}

impl Timeline {
    pub fn new(start: u32, clock_rate: u32) -> Self {
        Timeline {
            latest: Timestamp {
                timestamp: u64::from(start),
                start,
                clock_rate,
            },
            max_jump: MAX_TS_JUMP_SECS * clock_rate,
        }
    }

    pub fn advance(&mut self, rtp_timestamp: u32) -> Result<Timestamp, Error> {
        // TODO: error on u64 overflow.
        let ts_high_bits = self.latest.timestamp & 0xFFFF_FFFF_0000_0000;
        let new_ts = match rtp_timestamp < (self.latest.timestamp as u32) {
            true  => ts_high_bits + 1u64<<32 + u64::from(rtp_timestamp),
            false => ts_high_bits + u64::from(rtp_timestamp),
        };
        let forward_ts = crate::Timestamp {
            timestamp: new_ts,
            clock_rate: self.latest.clock_rate,
            start: self.latest.start,
        };
        let forward_delta = forward_ts.timestamp - self.latest.timestamp;
        if forward_delta > u64::from(self.max_jump) {
            let backward_ts = crate::Timestamp {
                timestamp: ts_high_bits + (self.latest.timestamp & 0xFFFF_FFFF) - u64::from(rtp_timestamp),
                clock_rate: self.latest.clock_rate,
                start: self.latest.start,
            };
            bail!("Timestamp jumped (forward by {} from {} to {}, more than allowed {} sec OR backward by {} from {} to {})",
                  forward_delta, self.latest.timestamp, new_ts, MAX_TS_JUMP_SECS,
                  self.latest.timestamp - backward_ts.timestamp, self.latest.timestamp, backward_ts);
        }
        self.latest = forward_ts;
        Ok(self.latest)
    }
}

impl Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (mod-2^32: {}), npt {:.03}",
               self.timestamp, self.timestamp as u32, ((self.timestamp - u64::from(self.start)) as f64) / (self.clock_rate as f64))
    }
}

impl Debug for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Context {
    pub local_addr: std::net::SocketAddr,
    pub peer_addr: std::net::SocketAddr,
    pub established: std::time::SystemTime,

    /// The byte position within the input stream. The bottom 32 bits can be compared to the TCP sequence number.
    pub rtsp_message_offset: u64,
}

struct Codec {
    ctx: Context,
}

/// Returns the range within `buf` that represents `subset`.
/// If `subset` is empty, returns None; otherwise panics if `subset` is not within `buf`.
fn as_range(buf: &BytesMut, subset: &[u8]) -> Option<std::ops::Range<usize>> {
    if subset.is_empty() {
        return None;
    }
    let subset_p = subset.as_ptr() as usize;
    let buf_p = buf.as_ptr() as usize;
    let off = match subset_p.checked_sub(buf_p) {
        Some(off) => off,
        None => panic!("{}-byte subset not within {}-byte buf", subset.len(), buf.len()),
    };
    let end = off + subset.len();
    assert!(end <= buf.len());
    Some(off..end)
}

impl tokio_util::codec::Decoder for Codec {
    type Item = ReceivedMessage;
    type Error = failure::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let (msg, len): (Message<&[u8]>, _) = match rtsp_types::Message::parse(src) {
            Ok((m, l)) => (m, l),
            Err(rtsp_types::ParseError::Error) => bail!("RTSP parse error: {:#?}", &self.ctx),
            Err(rtsp_types::ParseError::Incomplete) => return Ok(None),
        };

        // Map msg's body to a Bytes representation and advance `src`. Awkward:
        // 1.  lifetime concerns require mapping twice: first so the message
        //     doesn't depend on the BytesMut, which needs to be split/advanced;
        //     then to get the proper Bytes body in place post-split.
        // 2.  rtsp_types messages must be AsRef<[u8]>, so we can't use the
        //     range as an intermediate body.
        // 3.  within a match because the rtsp_types::Message enum itself
        //     doesn't have body/replace_body/map_body methods.
        let msg = match msg {
            Message::Request(msg) => {
                let body_range = as_range(src, msg.body());
                let msg = msg.replace_body(rtsp_types::Empty);
                if let Some(r) = body_range {
                    let mut raw_msg = src.split_to(len);
                    raw_msg.advance(r.start);
                    raw_msg.truncate(r.len());
                    Message::Request(msg.replace_body(raw_msg.freeze()))
                } else {
                    src.advance(len);
                    Message::Request(msg.replace_body(Bytes::new()))
                }
            },
            Message::Response(msg) => {
                let body_range = as_range(src, msg.body());
                let msg = msg.replace_body(rtsp_types::Empty);
                if let Some(r) = body_range {
                    let mut raw_msg = src.split_to(len);
                    raw_msg.advance(r.start);
                    raw_msg.truncate(r.len());
                    Message::Response(msg.replace_body(raw_msg.freeze()))
                } else {
                    src.advance(len);
                    Message::Response(msg.replace_body(Bytes::new()))
                }
            },
            Message::Data(msg) => {
                let body_range = as_range(src, msg.as_slice());
                let msg = msg.replace_body(rtsp_types::Empty);
                if let Some(r) = body_range {
                    let mut raw_msg = src.split_to(len);
                    raw_msg.advance(r.start);
                    raw_msg.truncate(r.len());
                    Message::Data(msg.replace_body(raw_msg.freeze()))
                } else {
                    src.advance(len);
                    Message::Data(msg.replace_body(Bytes::new()))
                }
            },
        };
        let msg = ReceivedMessage {
            ctx: self.ctx,
            msg,
        };
        self.ctx.rtsp_message_offset += u64::try_from(len).expect("usize fits in u64");
        Ok(Some(msg))
    }
}

impl tokio_util::codec::Encoder<rtsp_types::Message<bytes::Bytes>> for Codec {
    type Error = failure::Error;

    fn encode(&mut self, item: rtsp_types::Message<bytes::Bytes>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let mut w = std::mem::replace(dst, BytesMut::new()).writer();
        item.write(&mut w).expect("bytes Writer is infallible");
        *dst = w.into_inner();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}