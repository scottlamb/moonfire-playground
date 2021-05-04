//! RTP handling.

use bytes::Bytes;
use failure::{Error, bail};
use log::{debug, trace};
use pretty_hex::PrettyHex;
use rtp::packetizer::Marshaller;

#[derive(Debug)]
pub struct Packet {
    pub rtsp_ctx: crate::Context,
    pub timestamp: crate::Timestamp,
    pub pkt: rtp::packet::Packet,
}

pub trait PacketHandler {
    /// Handles a packet.
    /// `timestamp` is non-decreasing between calls.
    fn pkt(&mut self, pkt: Packet) -> Result<(), Error>;

    /// Handles the end of the stream.
    fn end(&mut self) -> Result<(), Error>;
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
pub struct StrictSequenceChecker<'a> {
    ssrc: u32,
    next_seq: u16,
    inner: &'a mut dyn PacketHandler,
    max_seq_skip: u16,
}

impl<'a> StrictSequenceChecker<'a> {
    pub fn new(ssrc: u32, next_seq: u16, inner: &'a mut dyn PacketHandler) -> Self {
        Self {
            ssrc,
            next_seq,
            inner,
            max_seq_skip: MAX_INITIAL_SEQ_SKIP,
        }
    }
}

impl<'a> super::ChannelHandler for StrictSequenceChecker<'a> {
    fn data(&mut self, rtsp_ctx: crate::Context, timeline: &mut crate::Timeline, data: Bytes) -> Result<(), Error> {
        let pkt = match rtp::packet::Packet::unmarshal(&data) {
            Err(e) => bail!("corrupt RTP packet while expecting seq={:04x} at {:#?}: {}", self.next_seq, &rtsp_ctx, e),
            Ok(p) => p,
        };
        let timestamp = match timeline.advance(pkt.header.timestamp) {
            Ok(ts) => ts,
            Err(e) => return Err(e.context(format!("timestamp error in seq={:04x} {:#?}", pkt.header.sequence_number, &rtsp_ctx)).into()),
        };
        if pkt.header.ssrc != self.ssrc
           || pkt.header.sequence_number.wrapping_sub(self.next_seq) > self.max_seq_skip {
            bail!("Expected ssrc={:08x} seq={:04x} got ssrc={:08x} seq={:04x} ts={} at {:#?}",
                  self.ssrc, self.next_seq, pkt.header.ssrc, pkt.header.sequence_number, timestamp, &rtsp_ctx);
        }
        debug!("pkt{} seq={:04x} ts={}", if pkt.header.marker { "   " } else { "(M)"}, self.next_seq, &timestamp);
        trace!("{:?}", data.hex_dump());
        self.next_seq = pkt.header.sequence_number.wrapping_add(1);
        self.max_seq_skip = 0;
        self.inner.pkt(Packet {
            rtsp_ctx,
            timestamp,
            pkt
        })
    }

    fn end(&mut self) -> Result<(), Error> {
        self.inner.end()
    }
}
