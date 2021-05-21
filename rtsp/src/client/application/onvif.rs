//! ONVIF metadata streams.
//! See the
//! [ONVIF Streaming Specification](https://www.onvif.org/specs/stream/ONVIF-Streaming-Spec.pdf)
//! version 19.12 section 5.2.1.1. The RTP layer muxing is simple: RTP packets with the MARK
//! bit set end messages.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use failure::{Error, bail};

use crate::client::DemuxedItem;

#[derive(Debug)]
pub enum CompressionType {
    Uncompressed,
    GzipCompressed,
    ExiDefault,
    ExiInBand,
}

#[derive(Debug)]
pub struct Parameters {
    pub compression_type: CompressionType,
}

impl Parameters {
    pub fn from(encoding_name: &str) -> Option<Parameters> {
        let compression_type = match encoding_name {
            "vnd.onvif.metadata" => CompressionType::Uncompressed,
            "vnd.onvif.metadata.gzip" => CompressionType::GzipCompressed,
            "vnd.onvif.metadata.exi.onvif" => CompressionType::ExiDefault,
            "vnd.onvif.metadata.exi.ext" => CompressionType::ExiInBand,
            _ => return None,
        };
        Some(Parameters {
            compression_type,
        })
    }
}

pub(crate) struct Demuxer {
    state: State,
    high_water_size: usize,
}

enum State {
    Idle,
    InProgress(InProgress),
    Ready(Message),
}

struct InProgress {
    ctx: crate::Context,
    timestamp: crate::Timestamp,
    data: BytesMut,
}

pub struct Message {
    pub ctx: crate::Context,
    pub timestamp: crate::Timestamp,
    pub data: Bytes,
}

impl Demuxer {
    pub(crate) fn new() -> Box<dyn crate::client::Demuxer> {
        Box::new(Demuxer {
            state: State::Idle,
            high_water_size: 0,
        })
    }
}

impl crate::client::Demuxer for Demuxer {
    fn push(&mut self, pkt: crate::client::rtp::Packet) -> Result<(), failure::Error> {
        let mut in_progress = match std::mem::replace(&mut self.state, State::Idle) {
            State::InProgress(in_progress) => {
                if in_progress.timestamp.timestamp != pkt.timestamp.timestamp {
                    bail!("Timestamp changed from {} to {} (@ seq {:04x}) with message in progress",
                        &in_progress.timestamp, &pkt.timestamp, pkt.sequence_number);
                }
                in_progress
            },
            State::Ready(..) => panic!("push while in state ready"),
            State::Idle => {
                if pkt.mark { // fast-path: avoid copy.
                    self.state = State::Ready(Message {
                        ctx: pkt.rtsp_ctx,
                        timestamp: pkt.timestamp,
                        data: pkt.payload,
                    });
                    return Ok(());
                }
                InProgress {
                    ctx: pkt.rtsp_ctx,
                    timestamp: pkt.timestamp,
                    data: BytesMut::with_capacity(self.high_water_size),
                }
            },
        };
        in_progress.data.put(pkt.payload);
        if pkt.mark {
            self.high_water_size = std::cmp::max(
                self.high_water_size,
                in_progress.data.remaining());
            self.state = State::Ready(Message {
                ctx: in_progress.ctx,
                timestamp: in_progress.timestamp,
                data: in_progress.data.freeze(),
            });
        } else {
            self.state = State::InProgress(in_progress);
        }
        Ok(())
    }

    fn pull(&mut self) -> Result<Option<DemuxedItem>, Error> {
        Ok(match std::mem::replace(&mut self.state, State::Idle) {
            State::Ready(message) => Some(DemuxedItem::Message(message)),
            s => {
                self.state = s;
                None
            },
        })
    }
}
