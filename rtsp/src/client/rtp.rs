//! RTP handling.

use bytes::Bytes;
use failure::{Error, bail};
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

const MAX_TS_JUMP_SECS: u32 = 10;

/// Ensures packets have the correct SSRC, are in sequence with no gaps, and have reasonable timestamps.
///
/// This is the simplest and easiest-to-debug policy. It may suffice for
/// connecting to an IP camera via RTP/AVP/TCP. We'll have to see if any IP
/// cameras have a race between the `seq` returned in the `RTP-Info` header
/// and sending the first RTP packet, if they skip sequence numbers when the
/// TCP window fills, etc.
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
    timestamp: crate::Timestamp,
    max_timestamp_jump: u32,
    inner: &'a mut dyn PacketHandler,
}

impl<'a> StrictSequenceChecker<'a> {
    pub fn new(ssrc: u32, next_seq: u16, start_timestamp: u32, clock_rate: u32, inner: &'a mut dyn PacketHandler) -> Self {
        Self {
            ssrc,
            next_seq,
            timestamp: crate::Timestamp {
                timestamp: u64::from(start_timestamp),
                start: start_timestamp,
                clock_rate,
            },
            max_timestamp_jump: MAX_TS_JUMP_SECS * clock_rate,
            inner,
        }
    }
}

impl<'a> super::ChannelHandler for StrictSequenceChecker<'a> {
    fn data(&mut self, rtsp_ctx: crate::Context, data: Bytes) -> Result<(), Error> {
        let pkt = match rtp::packet::Packet::unmarshal(&data) {
            Err(e) => bail!("corrupt RTP packet while expecting seq={:04x} at {:#?}: {}", self.next_seq, &rtsp_ctx, e),
            Ok(p) => p,
        };
        if pkt.header.ssrc != self.ssrc || pkt.header.sequence_number != self.next_seq {
            bail!("Expected ssrc={:08x} seq={:04x} got ssrc={:08x} seq={:04x} at {:#?}", self.ssrc, self.next_seq, pkt.header.ssrc, pkt.header.sequence_number, &rtsp_ctx);
        }
        // TODO: error on u64 overflow.
        let ts_high_bits = self.timestamp.timestamp & 0xFFFF_FFFF_0000_0000;
        let new_ts = match pkt.header.timestamp < (self.timestamp.timestamp as u32) {
            true  => ts_high_bits + 1u64<<32 + u64::from(pkt.header.timestamp),
            false => ts_high_bits + u64::from(pkt.header.timestamp),
        };
        let forward_ts = crate::Timestamp {
            timestamp: new_ts,
            clock_rate: self.timestamp.clock_rate,
            start: self.timestamp.start,
        };
        let forward_delta = forward_ts.timestamp - self.timestamp.timestamp;
        if forward_delta > u64::from(self.max_timestamp_jump) {
            let backward_ts = crate::Timestamp {
                timestamp: ts_high_bits + (self.timestamp.timestamp & 0xFFFF_FFFF) - (pkt.header.timestamp as u64),
                clock_rate: self.timestamp.clock_rate,
                start: self.timestamp.start,
            };
            bail!("Timestamp jumped (forward by {} from {} to {}, more than allowed {} sec OR backward by {} from {} to {}) at seq={:04x} {:#?}",
                  forward_delta, self.timestamp, new_ts, MAX_TS_JUMP_SECS,
                  self.timestamp.timestamp - backward_ts.timestamp, self.timestamp, backward_ts,
                  pkt.header.sequence_number, &rtsp_ctx);
        }
        self.next_seq = self.next_seq.wrapping_add(1);
        self.timestamp = forward_ts;
        self.inner.pkt(Packet {
            rtsp_ctx,
            timestamp: self.timestamp,
            pkt
        })
    }

    fn end(&mut self) -> Result<(), Error> {
        self.inner.end()
    }
}
