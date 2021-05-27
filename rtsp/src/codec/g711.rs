//! G.711 (PCMA and PCMU) support.
//! https://datatracker.ietf.org/doc/html/rfc3551#section-4.5.14
//! https://www.itu.int/rec/T-REC-G.711

use std::convert::TryFrom;
use std::num::NonZeroU32;

use bytes::Bytes;
use failure::Error;
use failure::format_err;

use super::CodecItem;

#[derive(Debug)]
pub(crate) struct Demuxer {
    parameters: super::Parameters,
    pending: Option<super::AudioFrame>,
}

impl Demuxer {
    pub(super) fn new(clock_rate: u32) -> Self {
        Self {
            parameters: super::Parameters::Audio(super::AudioParameters {
                rfc6381_codec: None,
                frame_length: None, // variable
                clock_rate,
                extra_data: Bytes::new(),
                config: super::AudioCodecConfig::Other,
            }),
            pending: None,
        }
    }

    pub(super) fn parameters(&self) -> Option<&super::Parameters> {
        Some(&self.parameters)
    }

    pub(super) fn push(&mut self, pkt: crate::client::rtp::Packet) -> Result<(), Error> {
        assert!(self.pending.is_none());

        // There is one byte per sample so the frame length in samples is simply the byte length.
        let frame_length = u32::try_from(pkt.payload.len())
            .map_err(|_| format_err!("crazy long G.711 payload ({} bytes)", pkt.payload.len()))?;
        let frame_length = NonZeroU32::new(frame_length)
            .ok_or_else(|| format_err!("zero-length G.711 payload"))?;
        self.pending = Some(super::AudioFrame {
            ctx: pkt.rtsp_ctx,
            stream_id: pkt.stream_id,
            timestamp: pkt.timestamp,
            frame_length,
            data: pkt.payload,
        });
        Ok(())
    }

    pub(super) fn pull(&mut self) -> Result<Option<super::CodecItem>, Error> {
        Ok(self.pending.take().map(|a| CodecItem::AudioFrame(a)))
    }
}
