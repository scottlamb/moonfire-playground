//! RTP handling.

use async_trait::async_trait;
use bytes::{Buf, Bytes};
use failure::{Error, bail, format_err};
use log::{debug, trace};
use pretty_hex::PrettyHex;

#[derive(Debug)]
pub struct Packet {
    pub rtsp_ctx: crate::Context,
    pub timestamp: crate::Timestamp,
    pub sequence_number: u16,
    pub mark: bool,
    pub payload: Bytes,
}

#[async_trait]
pub trait PacketHandler {
    /// Handles a packet.
    /// `timestamp` is non-decreasing between calls.
    async fn pkt(&mut self, pkt: Packet) -> Result<(), Error>;

    /// Handles the end of the stream.
    async fn end(&mut self) -> Result<(), Error>;
}

/// Maximum number of skipped initial sequence numbers.
/// At least with a [Dahua
/// IPC-HDW5442T-ZE](https://www.dahuasecurity.com/products/All-Products/Network-Cameras/WizMind-Series/5-Series/4MP/IPC-HDW5442T-ZE)
/// running `V2.800.15OG004.0.T, Build Date: 2020-11-23`, the first packet's sequence number is sometimes higher than the
/// that specified in the `PLAY` response's `RTP-Info: ...;seq=...` field. Perhaps this happens if the next IDR frame happens just
/// as the `PLAY` command is finishing.
const MAX_INITIAL_SEQ_SKIP: u16 = 128;

/// Ensures packets have the correct SSRC, are in sequence with (almost) no gaps, and have reasonable timestamps.
///
/// Exception: it allows a gap in the sequence at the beginning, as explained at [`MAX_INITIAL_SEQ_SKIP`].
///
/// This is the simplest and easiest-to-debug policy. It may suffice for
/// connecting to an IP camera via RTP/AVP/TCP. We'll have to see if cameras
/// skip sequence numbers in any other cases, such as when the TCP window fills
/// and/or the camera is overloaded.
///
/// It definitely wouldn't work well when using UDP or when using a proxy which
/// may be using UDP for the backend:
/// *   while TCP handles lost, duplicated, and out-of-order packets for us, UDP doesn't.
/// *   there might be packets still flowing to that address from a previous RTSP session.
///
/// At least [one camera](https://github.com/scottlamb/moonfire-nvr/wiki/Cameras:-Reolink#reolink-rlc-410-hardware-version-ipc_3816m)
/// sometimes still sends data from old RTSP sessions over new ones. This seems
/// like a serious bug, but we could work around it by discarding those packets
/// by SSRC rather than erroring out.
///
/// [RFC 3550 section 8.2](https://tools.ietf.org/html/rfc3550#section-8.2) says that SSRC
/// can change mid-session with a RTCP BYE message. This currently isn't handled. I'm
/// not sure it will ever come up with IP cameras.
pub struct StrictSequenceChecker<P: PacketHandler> {
    ssrc: u32,
    next_seq: u16,
    inner: P,
    max_seq_skip: u16,
}

impl<P: PacketHandler> StrictSequenceChecker<P> {
    pub fn new(ssrc: u32, next_seq: u16, inner: P) -> Self {
        Self {
            ssrc,
            next_seq,
            inner,
            max_seq_skip: MAX_INITIAL_SEQ_SKIP,
        }
    }

    pub fn into_inner(self) -> P {
        self.inner
    }
}

#[async_trait]
impl<P: PacketHandler + Send> super::ChannelHandler for StrictSequenceChecker<P> {
    async fn data(&mut self, rtsp_ctx: crate::Context, timeline: &mut crate::Timeline, mut data: Bytes) -> Result<(), Error> {
        let reader = rtp_rs::RtpReader::new(&data[..])
            .map_err(|e| format_err!("corrupt RTP header while expecting seq={:04x} at {:#?}: {:?}", self.next_seq, &rtsp_ctx, e))?;
        let sequence_number = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = match timeline.advance(reader.timestamp()) {
            Ok(ts) => ts,
            Err(e) => return Err(e.context(format!("timestamp error in seq={:04x} {:#?}", sequence_number, &rtsp_ctx)).into()),
        };
        let ssrc = reader.ssrc();
        if ssrc != self.ssrc
           || sequence_number.wrapping_sub(self.next_seq) > self.max_seq_skip {
            bail!("Expected ssrc={:08x} seq={:04x} got ssrc={:08x} seq={:04x} ts={} at {:#?}",
                  self.ssrc, self.next_seq, ssrc, sequence_number, timestamp, &rtsp_ctx);
        }
        let mark = reader.mark();
        debug!("pkt{} seq={:04x} ts={}", if mark { "   " } else { "(M)"}, self.next_seq, &timestamp);
        trace!("{:?}", data.hex_dump());
        let payload_range = crate::as_range(&data, reader.payload())
            .ok_or_else(|| format_err!("empty paylaod"))?;
        data.truncate(payload_range.end);
        data.advance(payload_range.start);
        self.next_seq = sequence_number.wrapping_add(1);
        self.max_seq_skip = 0;
        self.inner.pkt(Packet {
            rtsp_ctx,
            timestamp,
            sequence_number,
            mark,
            payload: data,
        }).await
    }

    async fn end(&mut self) -> Result<(), Error> {
        self.inner.end().await
    }
}
