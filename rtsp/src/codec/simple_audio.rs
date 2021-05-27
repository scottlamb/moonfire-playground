//! Fixed-size audio sample codecs, including G.711 (PCMA and PCMU), L8, and L16.
//! https://datatracker.ietf.org/doc/html/rfc3551#section-4.5

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
    shift: u32,
    mask: u32,
}

impl Demuxer {
    /// Creates a new Demuxer.
    /// `shift` is represents the size of a sample: 0 for 1 byte, 1 for 2 bytes, 2 for 4 bytes, etc.
    pub(super) fn new(clock_rate: u32, shift: u32) -> Self {
        Self {
            parameters: super::Parameters::Audio(super::AudioParameters {
                rfc6381_codec: None,
                frame_length: None, // variable
                clock_rate,
                extra_data: Bytes::new(),
                config: super::AudioCodecConfig::Other,
            }),
            shift,
            mask: (1 << shift) - 1,
            pending: None,
        }
    }

    pub(super) fn parameters(&self) -> Option<&super::Parameters> {
        Some(&self.parameters)
    }

    fn frame_length(&self, payload_len: usize) -> Option<NonZeroU32> {
        let len = u32::try_from(payload_len).ok()?;
        if (len & self.mask) != 0 {
            return None;
        }
        NonZeroU32::new(len >> self.shift)
    }

    pub(super) fn push(&mut self, pkt: crate::client::rtp::Packet) -> Result<(), Error> {
        assert!(self.pending.is_none());
        let frame_length = self.frame_length(pkt.payload.len())
            .ok_or_else(|| format_err!("invalid length {} for payload of {}-byte audio samples",
                                       pkt.payload.len(), 1 << self.shift))?;
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
