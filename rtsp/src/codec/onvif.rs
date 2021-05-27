//! ONVIF metadata streams.
//! See the
//! [ONVIF Streaming Specification](https://www.onvif.org/specs/stream/ONVIF-Streaming-Spec.pdf)
//! version 19.12 section 5.2.1.1. The RTP layer muxing is simple: RTP packets with the MARK
//! bit set end messages.

use bytes::{Buf, BufMut, BytesMut};
use failure::{Error, bail};

use super::CodecItem;

#[derive(Clone, Debug)]
pub enum CompressionType {
    Uncompressed,
    GzipCompressed,
    ExiDefault,
    ExiInBand,
}

#[derive(Debug)]
pub(crate) struct Demuxer {
    parameters: super::Parameters,
    state: State,
    high_water_size: usize,
}

#[derive(Debug)]
enum State {
    Idle,
    InProgress(InProgress),
    Ready(super::MessageFrame),
}

#[derive(Debug)]
struct InProgress {
    ctx: crate::Context,
    timestamp: crate::Timestamp,
    data: BytesMut,
}

impl Demuxer {
    pub(super) fn new(compression_type: CompressionType) -> Self {
        Demuxer {
            parameters: super::Parameters::Message(super::MessageParameters(compression_type)),
            state: State::Idle,
            high_water_size: 0,
        }
    }

    pub(super) fn parameters(&self) -> Option<&super::Parameters> {
        Some(&self.parameters)
    }

    pub(super) fn push(&mut self, pkt: crate::client::rtp::Packet) -> Result<(), failure::Error> {
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
                    self.state = State::Ready(super::MessageFrame {
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
            self.state = State::Ready(super::MessageFrame {
                ctx: in_progress.ctx,
                timestamp: in_progress.timestamp,
                data: in_progress.data.freeze(),
            });
        } else {
            self.state = State::InProgress(in_progress);
        }
        Ok(())
    }

    pub(super) fn pull(&mut self) -> Result<Option<CodecItem>, Error> {
        Ok(match std::mem::replace(&mut self.state, State::Idle) {
            State::Ready(message) => Some(CodecItem::MessageFrame(message)),
            s => {
                self.state = s;
                None
            },
        })
    }
}
