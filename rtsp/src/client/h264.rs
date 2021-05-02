//! Depacketizes H.264.
//! The rtp crate also has H.264 depacketization logic, but it doesn't seem to match my use case. I want to
//! iterate the NALs, not re-encode them in Annex B format.
//! https://docs.rs/rtp/0.2.2/rtp/codecs/h264/struct.H264Packet.html

use bytes::{Bytes, BytesMut, Buf, BufMut};
use failure::{Error, bail};

#[derive(Debug)]
pub struct NalType {
    pub name: &'static str,
    pub is_vcl: bool,
}

// See Table 7-1, PDF page 85 of
// [ISO/IEC 14496-10:2014(E)](https://github.com/scottlamb/moonfire-nvr/wiki/Standards-and-specifications#video-codecs).
pub const NAL_TYPES: [Option<NalType>; 32] = [
    /*  0 */ None,
    /*  1 */ Some(NalType { name: "slice_layer_without_partitioning", is_vcl: true }),
    /*  2 */ Some(NalType { name: "slice_data_partition_a_layer",     is_vcl: true }),
    /*  3 */ Some(NalType { name: "slice_data_partition_b_layer",     is_vcl: true }),
    /*  4 */ Some(NalType { name: "slice_data_partition_c_layer",     is_vcl: true }),
    /*  5 */ Some(NalType { name: "slice_layer_without_partitioning", is_vcl: true }),
    /*  6 */ Some(NalType { name: "sei",                              is_vcl: false }),
    /*  7 */ Some(NalType { name: "seq_parameter_set",                is_vcl: false }),
    /*  8 */ Some(NalType { name: "pic_parameter_set",                is_vcl: false }),
    /*  9 */ Some(NalType { name: "access_unit_delimiter",            is_vcl: false }),
    /* 10 */ Some(NalType { name: "end_of_seq",                       is_vcl: false }),
    /* 11 */ Some(NalType { name: "end_of_stream",                    is_vcl: false }),
    /* 12 */ Some(NalType { name: "filler_data",                      is_vcl: false }),
    /* 13 */ Some(NalType { name: "seq_parameter_set_extension",      is_vcl: false }),
    /* 14 */ Some(NalType { name: "prefix_nal_unit",                  is_vcl: false }),
    /* 15 */ Some(NalType { name: "subset_seq_parameter_set",         is_vcl: false }),
    /* 16 */ Some(NalType { name: "depth_parameter_set",              is_vcl: false }),
    /* 17 */ None,
    /* 18 */ None,
    /* 19 */ Some(NalType { name: "slice_layer_without_partitioning", is_vcl: false }),
    /* 20 */ Some(NalType { name: "slice_layer_extension",            is_vcl: false }),
    /* 21 */ Some(NalType { name: "slice_layer_extension_for_3d",     is_vcl: false }),
    /* 22 */ None,
    /* 23 */ None,
    /* 24 */ None,
    /* 25 */ None,
    /* 26 */ None,
    /* 27 */ None,
    /* 28 */ None,
    /* 29 */ None,
    /* 30 */ None,
    /* 31 */ None,
];

/// A [super::rtp::PacketHandler] implementation which breaks H.264 data into access units and NALs.
/// Currently expects that the stream starts at an access unit boundary and has no lost packets.
pub struct Handler<'a> {
    inner: &'a mut dyn AccessUnitHandler,

    state: State,

    /// The largest fragment used. This is used for the buffer capacity on subsequent fragments, minimizing reallocation.
    frag_high_water: usize,
}

struct PreMark {
    timestamp: crate::Timestamp,

    /// If a FU-A fragment is in progress, the buffer used to accumulate the NAL.
    frag_buf: Option<BytesMut>,
}

enum State {
    /// Not currently processing an access unit.
    Inactive,

    /// Currently processing an access unit.
    /// This will be flushed after a marked packet or when receiving a later timestamp.
    PreMark(PreMark),

    /// Finished processing the given packet. It's an error to receive the same timestamp again.
    PostMark { timestamp: crate::Timestamp },
}

pub trait AccessUnitHandler {
    fn start(&mut self, rtsp_ctx: &crate::Context, timestamp: crate::Timestamp, hdr: &rtp::header::Header) -> Result<(), Error>;
    fn nal(&mut self, nal: Bytes) -> Result<(), Error>;
    fn end(&mut self) -> Result<(), Error>;
}

pub struct NopAccessUnitHandler;

impl AccessUnitHandler for NopAccessUnitHandler {
    fn start(&mut self, _rtsp_ctx: &crate::Context, timestamp: crate::Timestamp, _hdr: &rtp::header::Header) -> Result<(), Error> {
        println!("access unit with timestamp {}:", timestamp);
        Ok(())
    }

    fn nal(&mut self, nalu: Bytes) -> Result<(), Error> {
        let nal_ref_idc = nalu[0] & 0b0110_0000 >> 5;
        let nal_type_code = nalu[0] & 0b0001_1111;
        let nal_type = &NAL_TYPES[usize::from(nal_type_code)];
        println!("  nal ref_idc {} type {:?}", nal_ref_idc, nal_type.as_ref().map(|n| n.name));
        Ok(())
    }

    fn end(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

impl<'a> Handler<'a> {
    pub fn new(inner: &'a mut dyn AccessUnitHandler) -> Self {
        Handler {
            inner,
            state: State::Inactive,
            frag_high_water: 0,
        }
    }
}

impl<'a> super::rtp::PacketHandler for Handler<'a> {
    fn end(&mut self) -> Result<(), Error> {
        if let State::PostMark{..} = self.state {
            self.inner.end()?;
        }
        Ok(())
    }

    fn pkt(&mut self, pkt: super::rtp::Packet) -> Result<(), Error> {
        let seq = pkt.pkt.header.sequence_number;
        let mut premark = match std::mem::replace(&mut self.state, State::Inactive) {
            State::Inactive => {
                self.inner.start(&pkt.rtsp_ctx, pkt.timestamp, &pkt.pkt.header)?;
                PreMark {
                    timestamp: pkt.timestamp,
                    frag_buf: None
                }
            },
            State::PreMark(state) => {
                if state.timestamp.timestamp != pkt.timestamp.timestamp {
                    if state.frag_buf.is_some() {
                        bail!("Timestamp changed from {} to {} in the middle of a fragmented NAL at seq={:04x} {:#?}", state.timestamp, pkt.timestamp, seq, &pkt.rtsp_ctx);
                    }
                    self.inner.end()?;
                }
                state
            },
            State::PostMark { timestamp: state_ts } => {
                if state_ts.timestamp == pkt.timestamp.timestamp {
                    bail!("Received packet with timestamp {} after marked packet with same timestamp at seq={:04x} {:#?}", pkt.timestamp, seq, &pkt.rtsp_ctx);
                }
                self.inner.end()?;
                self.inner.start(&pkt.rtsp_ctx, pkt.timestamp, &pkt.pkt.header)?;
                PreMark {
                    timestamp: pkt.timestamp,
                    frag_buf: None,
                }
            }
        };

        let mut data = pkt.pkt.payload;
        if data.is_empty() {
            bail!("Empty NAL at RTP seq {:04x}, {:#?}", seq, &pkt.rtsp_ctx);
        }
        // https://tools.ietf.org/html/rfc6184#section-5.2
        let nal_header = data[0];
        if (nal_header >> 7) != 0 {
            bail!("NAL header has F bit set at seq {:04x} {:#?}", seq, &pkt.rtsp_ctx);
        }
        match nal_header & 0b11111 {
            1..=23 => {
                if premark.frag_buf.is_some() {
                    bail!("Non-fragmented NAL while fragment in progress seq {:04x} {:#?}", seq, &pkt.rtsp_ctx);
                }
                self.inner.nal(data)?;
            },
            24..=27 | 29 => unimplemented!("unimplemented NAL (header {:02x}) at seq {:04x} {:#?}", nal_header, seq, &pkt.rtsp_ctx),
            28 => {
                // FU-A. https://tools.ietf.org/html/rfc6184#section-5.8
                if data.len() < 3 {
                    bail!("FU-A is too short at seq {:04x} {:#?}", seq, &pkt.rtsp_ctx);
                }
                let fu_header = data[1];
                let start    = (fu_header & 0b10000000) != 0;
                let end      = (fu_header & 0b01000000) != 0;
                let reserved = (fu_header & 0b00100000) != 0;
                let nal_header = (nal_header & 0b011100000) | (fu_header & 0b00011111);
                //println!("seq {:04x} FU-A, {:08b} {:08b}", seq, nal_header, fu_header);
                if (start && end) || reserved {
                    bail!("Invalid FU-A header {:x} at seq {:04x} {:#?}", fu_header, seq, &pkt.rtsp_ctx);
                }
                match (start, premark.frag_buf.take()) {
                    (true, Some(_)) => bail!("FU-A with start bit while frag in progress at seq {:04x} {:#?}", seq, &pkt.rtsp_ctx),
                    (true, None) => {
                        let mut frag_buf = BytesMut::with_capacity(std::cmp::max(self.frag_high_water, data.len() - 1));
                        frag_buf.put_u8(nal_header);
                        data.advance(2);
                        frag_buf.put(data);
                        premark.frag_buf = Some(frag_buf);
                    },
                    (false, Some(mut frag_buf)) => {
                        if frag_buf[0] != nal_header {
                            bail!("FU-A has inconsistent NAL type: {:08b} then {:08b} at seq {:04x} {:#?}", frag_buf[0], nal_header, seq, &pkt.rtsp_ctx);
                        }
                        data.advance(2);
                        frag_buf.put(data);
                        if end {
                            self.frag_high_water = frag_buf.len();
                            self.inner.nal(frag_buf.freeze())?;
                        } else if pkt.pkt.header.marker {
                            bail!("FU-A with MARK and no END at seq {:04x} {:#?}", seq, pkt.rtsp_ctx);
                        } else {
                            premark.frag_buf = Some(frag_buf);
                        }
                    },
                    (false, None) => bail!("FU-A with start bit unset while no frag in progress at {:04x} {:#?}", seq, &pkt.rtsp_ctx),
                }
            },
            _ => bail!("bad nal header {:0x} at seq {:04x} {:#?}", nal_header, seq, &pkt.rtsp_ctx),
        }
        if pkt.pkt.header.marker {
            self.state = State::PostMark { timestamp: pkt.timestamp };
        } else {
            self.state = State::PreMark(premark);
        }
        Ok(())
    }
}
