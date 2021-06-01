//! RTP handling.

use bytes::{Buf, Bytes};
use failure::{Error, bail, format_err};
use log::trace;
use pretty_hex::PrettyHex;

/// An RTP packet.
#[derive(Debug)]
pub struct Packet {
    pub rtsp_ctx: crate::Context,
    pub stream_id: usize,
    pub timestamp: crate::Timestamp,
    pub sequence_number: u16,

    /// Number of skipped sequence numbers since the last packet.
    ///
    /// In the case of the first packet on the stream, this may also report loss
    /// packets since the `RTP-Info` header's `seq` value. However, currently
    /// that header is not required to be present and may be ignored (see
    /// [`crate::client::PlayPolicy::ignore_zero_seq()`].)
    pub loss: u16,

    pub mark: bool,

    /// Guaranteed to be less than u16::MAX bytes.
    pub payload: Bytes,
}

/// An RTCP sender report.
#[derive(Debug)]
pub struct SenderReport {
    pub stream_id: usize,
    pub rtsp_ctx: crate::Context,
    pub timestamp: crate::Timestamp,
    pub ntp_timestamp: crate::NtpTimestamp,
}

/// RTP demarshaller which ensures packets have the correct SSRC and monotonically increasing SEQ.
///
/// This reports packet loss (via [Packet::loss]) but doesn't prohibit it, except for losses
/// of more than `i16::MAX` which would be indistinguishable from non-monotonic sequence numbers.
/// Servers sometimes drop packets internally even when sending data via TCP.
///
/// At least [one camera](https://github.com/scottlamb/moonfire-nvr/wiki/Cameras:-Reolink#reolink-rlc-410-hardware-version-ipc_3816m)
/// sometimes sends data from old RTSP sessions over new ones. This seems like a
/// serious bug, and currently `StrictSequenceChecker` will error in this case,
/// although it'd be possible to discard the incorrect SSRC instead.
///
/// [RFC 3550 section 8.2](https://tools.ietf.org/html/rfc3550#section-8.2) says that SSRC
/// can change mid-session with a RTCP BYE message. This currently isn't handled. I'm
/// not sure it will ever come up with IP cameras.
#[derive(Debug)]
pub(super) struct StrictSequenceChecker {
    ssrc: Option<u32>,
    next_seq: Option<u16>,
}

impl StrictSequenceChecker {
    pub(super) fn new(ssrc: Option<u32>, next_seq: Option<u16>) -> Self {
        Self {
            ssrc,
            next_seq,
        }
    }

    pub(super) fn process(&mut self, rtsp_ctx: crate::Context, timeline: &mut super::Timeline,
                          stream_id: usize, mut data: Bytes) -> Result<Packet, Error> {
        // Terrible hack to try to make sense of the GW Security GW4089IP's audio stream.
        // It appears to have one RTSP interleaved message wrapped in another. RTP and RTCP
        // packets can never start with '$', so this shouldn't interfere with well-behaved
        // servers.
        if data.len() > 4 && data[0] == b'$'
           && usize::from(u16::from_be_bytes([data[2], data[3]])) <= data.len() - 4
        {
            log::debug!("stripping extra interleaved data header");
            data.advance(4);
            // also remove suffix? dunno.
        }

        let reader = rtp_rs::RtpReader::new(&data[..])
            .map_err(|e| format_err!(
                "corrupt RTP header while expecting seq={:04x?} at {:#?}: {:?}\n{:#?}",
                self.next_seq, &rtsp_ctx, e, data.hex_dump()))?;
        let sequence_number = u16::from_be_bytes([data[2], data[3]]); // I don't like rtsp_rs::Seq.
        let timestamp = match timeline.advance_to(reader.timestamp()) {
            Ok(ts) => ts,
            Err(e) => return Err(e.context(format!("timestamp error in stream {} seq={:04x} {:#?}",
                                                   stream_id, sequence_number, &rtsp_ctx)).into()),
        };
        let ssrc = reader.ssrc();
        let loss = sequence_number.wrapping_sub(self.next_seq.unwrap_or(sequence_number));
        if matches!(self.ssrc, Some(s) if s != ssrc) || loss > 0x80_00 {
            bail!("Expected ssrc={:08x?} seq={:04x?} got ssrc={:08x} seq={:04x} ts={} at {:#?}",
                  self.ssrc, self.next_seq, ssrc, sequence_number, timestamp, &rtsp_ctx);
        }
        self.ssrc = Some(ssrc);
        let mark = reader.mark();
        let payload_range = crate::as_range(&data, reader.payload())
            .ok_or_else(|| format_err!("empty payload at {:#?}", &rtsp_ctx))?;
        trace!("{:?} pkt {:04x}{} ts={} len={}", &rtsp_ctx, sequence_number,
               if mark { "   " } else { "(M)"}, &timestamp, payload_range.len());
        data.truncate(payload_range.end);
        data.advance(payload_range.start);
        self.next_seq = Some(sequence_number.wrapping_add(1));
        return Ok(Packet {
            stream_id,
            rtsp_ctx,
            timestamp,
            sequence_number,
            loss,
            mark,
            payload: data,
        })
    }
}
