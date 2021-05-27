//! Codec-specific logic (for audio, video, and application media types).
//!
//! Currently this primarily consists of RTP depacketization logic for each
//! codec, as needed for a client during `PLAY` and a server during `RECORD`.
//! Packetization (needed for the reverse) may be added in the future.

use std::num::{NonZeroU16, NonZeroU32};

use bytes::{Buf, Bytes};
use failure::{Error, bail};
use pretty_hex::PrettyHex;
use crate::client::rtp;

pub mod aac;
pub mod h264;
pub mod onvif;
pub mod g711;

pub enum CodecItem {
    VideoFrame(VideoFrame),
    AudioFrame(AudioFrame),
    MessageFrame(MessageFrame),
    SenderReport(crate::client::rtp::SenderReport),
}

#[derive(Clone, Debug)]
pub enum Parameters {
    Video(VideoParameters),
    Audio(AudioParameters),
    Message(MessageParameters),
}

#[derive(Clone)]
pub struct VideoParameters {
    pixel_dimensions: (u32, u32),
    rfc6381_codec: String,
    pixel_aspect_ratio: Option<(u32, u32)>,
    frame_rate: Option<(u32, u32)>,
    extra_data: Bytes,
}

impl VideoParameters {
    /// Returns a codec description in
    /// [RFC-6381](https://tools.ietf.org/html/rfc6381) form, eg `avc1.4D401E`.
    // TODO: use https://github.com/dholroyd/rfc6381-codec crate once published?
    pub fn rfc6381_codec(&self) -> &str {
        &self.rfc6381_codec
    }

    /// Returns the overall dimensions of the video frame in pixels, as `(width, height)`.
    pub fn pixel_dimensions(&self) -> (u32, u32) {
        self.pixel_dimensions
    }

    /// Returns the displayed size of a pixel, if known, as a dimensionless ratio `(h_spacing, v_spacing)`.
    /// This is as specified in [ISO/IEC 14496-12:2015](https://standards.iso.org/ittf/PubliclyAvailableStandards/c068960_ISO_IEC_14496-12_2015.zip])
    /// section 12.1.4.
    ///
    /// It's common for IP cameras to use [anamorphic](https://en.wikipedia.org/wiki/Anamorphic_format) sub streams.
    /// Eg a 16x9 camera may export the same video source as a 1920x1080 "main"
    /// stream and a 704x480 "sub" stream, without cropping. The former has a
    /// pixel aspect ratio of `(1, 1)` while the latter has a pixel aspect ratio
    /// of `(40, 33)`.
    pub fn pixel_aspect_ratio(&self) -> Option<(u32, u32)> {
        self.pixel_aspect_ratio
    }

    /// Returns the maximum frame rate in seconds as `(numerator, denominator)`,
    /// if known.
    ///
    /// May not be minimized, and may not be in terms of the clock rate. Eg 15
    /// frames per second might be returned as `(1, 15)` or `(6000, 90000)`. The
    /// standard NTSC framerate (roughly 29.97 fps) might be returned as
    /// `(1001, 30000)`.
    ///
    /// TODO: maybe return in clock rate units instead?
    /// TODO: expose fixed vs max distinction (see H.264 fixed_frame_rate_flag).
    pub fn frame_rate(&self) -> Option<(u32, u32)> {
        self.frame_rate
    }

    /// The codec-specific "extra data" to feed to eg ffmpeg to decode the video frames.
    /// *   H.264: an AvcDecoderConfig.
    pub fn extra_data(&self) -> &Bytes {
        &self.extra_data
    }
}

impl std::fmt::Debug for VideoParameters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoParameters")
         .field("rfc6381_codec", &self.rfc6381_codec)
         .field("pixel_dimensions", &self.pixel_dimensions)
         .field("pixel_aspect_ratio", &self.pixel_aspect_ratio)
         .field("frame_rate", &self.frame_rate)
         .field("extra_data", &self.extra_data.hex_dump())
         .finish()
    }
}

#[derive(Clone)]
pub struct AudioParameters {
    rfc6381_codec: Option<String>,
    frame_length: Option<NonZeroU32>,
    clock_rate: u32,
    extra_data: Bytes,
    config: AudioCodecConfig,
}

#[derive(Clone)]
enum AudioCodecConfig {
    Aac(aac::AudioSpecificConfig),
    Other,
}

impl std::fmt::Debug for AudioParameters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioParameters")
         .field("rfc6381_codec", &self.rfc6381_codec)
         .field("frame_length", &self.frame_length)
         .field("extra_data", &self.extra_data.hex_dump())
         .finish()
    }
}

impl AudioParameters {
    pub fn rfc6381_codec(&self) -> Option<&str> {
        self.rfc6381_codec.as_deref()
    }

    /// The length of each frame (in clock_rate units), if fixed.
    pub fn frame_length(&self) -> Option<NonZeroU32> {
        self.frame_length
    }

    pub fn clock_rate(&self) -> u32 {
        self.clock_rate
    }

    /// The codec-specific "extra data" to feed to eg ffmpeg to decode the audio.
    /// *   AAC: a serialized `AudioSpecificConfig`.
    pub fn extra_data(&self) -> &Bytes {
        &self.extra_data
    }

    /// Builds an `.mp4` `AudioSampleEntry` box (as defined in ISO/IEC 14496-12) if possible.
    /// Not all codecs can be placed into a `.mp4` file, and even for supported codecs there
    /// may be unsupported edge cases.
    pub fn sample_entry(&self) -> Result<Bytes, Error> {
        aac::get_mp4a_box(self)
    }
}

/// An audio frame, which consists of one or more samples.
pub struct AudioFrame {
    ctx: crate::Context,
    pub stream_id: usize,
    pub timestamp: crate::Timestamp,
    pub frame_length: NonZeroU32,

    // TODO: expose bytes or Buf (for zero-copy)?
    pub data: Bytes,
}

impl std::fmt::Debug for AudioFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioFrame")
         .field("stream_id", &self.stream_id)
         .field("ctx", &self.ctx)
         .field("timestamp", &self.timestamp)
         .field("frame_length", &self.frame_length)
         .field("data", &self.data.hex_dump()) 
         .finish()
    }
}

impl Buf for AudioFrame {
    fn remaining(&self) -> usize {
        self.data.remaining()
    }

    fn chunk(&self) -> &[u8] {
        self.data.chunk()
    }

    fn advance(&mut self, cnt: usize) {
        self.data.advance(cnt)
    }
}

#[derive(Clone, Debug)]
pub struct MessageParameters(onvif::CompressionType);

pub struct MessageFrame {
    pub ctx: crate::Context,
    pub timestamp: crate::Timestamp,

    // TODO: expose bytes or Buf (for zero-copy)?
    pub data: Bytes,
}

impl std::fmt::Debug for MessageFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioFrame")
         .field("ctx", &self.ctx)
         .field("timestamp", &self.timestamp)
         .field("data", &self.data.hex_dump())
         .finish()
    }
}

/// A single encoded video frame (aka picture, video sample, or video access unit).
///
/// Use the [bytes::Buf] implementation to retrieve data. Durations aren't
/// specified here; they can be calculated from the timestamp of a following
/// picture, or approximated via the frame rate.
pub struct VideoFrame {
    pub new_parameters: Option<VideoParameters>,

    pub ctx: crate::Context,

    /// This picture's timestamp in the time base associated with the stream.
    pub timestamp: crate::Timestamp,

    pub stream_id: usize,

    /// If this is a "random access point (RAP)" aka "instantaneous decoding refresh (IDR)" picture.
    /// The former is defined in ISO/IEC 14496-12; the latter in H.264. Both mean that this picture
    /// can be decoded without any other AND no pictures following this one depend on any pictures
    /// before this one.
    pub is_random_access_point: bool,

    /// If no other pictures require this one to be decoded correctly.
    /// In H.264 terms, this is a frame with `nal_ref_idc == 0`.
    pub is_disposable: bool,

    /// Position within `concat(data_prefix, data)`.
    pos: u32,

    data_prefix: [u8; 4],

    /// Frame content in the requested format. Currently in a single [bytes::Bytes]
    /// allocation, but this may change when supporting H.264 partitioned slices
    /// or if we revise the fragmentation implementation.
    data: bytes::Bytes,
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        //use pretty_hex::PrettyHex;
        f.debug_struct("VideoFrame")
         .field("timestamp", &self.timestamp)
         .field("ctx", &self.ctx)
         .field("new_parameters", &self.new_parameters)
         .field("is_random_access_point", &self.is_random_access_point)
         .field("is_disposable", &self.is_disposable)
         .field("pos", &self.pos)
         .field("data_len", &(self.data.len() + 4))
         //.field("data", &self.data.hex_dump()) 
         .finish()
    }
}

impl bytes::Buf for VideoFrame {
    fn remaining(&self) -> usize {
        self.data.len() + 4 - (self.pos as usize)
    }

    fn chunk(&self) -> &[u8] {
        let pos = self.pos as usize;
        if let Some(pos_within_data) = pos.checked_sub(4) {
            &self.data[pos_within_data..]
        } else {
            &self.data_prefix[pos..]
        }
    }

    fn advance(&mut self, cnt: usize) {
        assert!((self.pos as usize) + cnt <= 4 + self.data.len());
        self.pos += cnt as u32;
    }

    fn chunks_vectored<'a>(&'a self, dst: &mut [std::io::IoSlice<'a>]) -> usize {
        match dst.len() {
            0 => 0,
            1 => {
                dst[0] = std::io::IoSlice::new(self.chunk());
                1
            },
            _ if self.pos < 4 => {
                dst[0] = std::io::IoSlice::new(&self.data_prefix[self.pos as usize..]);
                dst[1] = std::io::IoSlice::new(&self.data);
                2
            },
            _ => {
                dst[0] = std::io::IoSlice::new(&self.data[(self.pos - 4) as usize..]);
                1
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum Demuxer {
    Aac(aac::Demuxer),
    G711(g711::Demuxer),
    H264(h264::Demuxer),
    Onvif(onvif::Demuxer),
}

impl Demuxer {
    pub(crate) fn new(
        media: &str,
        encoding_name: &str,
        clock_rate: u32,
        channels: Option<NonZeroU16>,
        format_specific_params: Option<&str>)
    -> Result<Self, Error> {
        use onvif::CompressionType;
        match (media, encoding_name) {
            ("video", "h264") => Ok(
                Demuxer::H264(h264::Demuxer::new(clock_rate, format_specific_params)?)
            ),
            ("audio", "mpeg4-generic") => Ok(
                Demuxer::Aac(aac::Demuxer::new(clock_rate, channels, format_specific_params)?)
            ),
            ("audio", "pcma") | ("audio", "pcmu") => Ok(
                Demuxer::G711(g711::Demuxer::new(clock_rate))
            ),
            ("application", "vnd.onvif.metadata") => Ok(
                Demuxer::Onvif(onvif::Demuxer::new(CompressionType::Uncompressed))
            ),
            ("application", "vnd.onvif.metadata.gzip") => Ok(
                Demuxer::Onvif(onvif::Demuxer::new(CompressionType::GzipCompressed))
            ),
            ("application", "vnd.onvif.metadata.exi.onvif") => Ok(
                Demuxer::Onvif(onvif::Demuxer::new(CompressionType::ExiDefault))
            ),
            ("application", "vnd.onvif.metadata.exi.ext") => Ok(
                Demuxer::Onvif(onvif::Demuxer::new(CompressionType::ExiInBand))
            ),
            (_, _) => {
                log::info!("no demuxer for media/encoding_name {}/{}", media, encoding_name);
                bail!("no demuxer for media/encoding_name {}/{}", media, encoding_name);
            },
        }
    }

    pub(crate) fn parameters(&self) -> Option<&Parameters> {
        match self {
            Demuxer::Aac(d) => d.parameters(),
            Demuxer::G711(d) => d.parameters(),
            Demuxer::H264(d) => d.parameters(),
            Demuxer::Onvif(d) => d.parameters(),
        }
    }

    pub(crate) fn push(&mut self, input: rtp::Packet) -> Result<(), Error> {
        match self {
            Demuxer::Aac(d) => d.push(input),
            Demuxer::G711(d) => d.push(input),
            Demuxer::H264(d) => d.push(input),
            Demuxer::Onvif(d) => d.push(input),
        }
    }

    pub(crate) fn pull(&mut self) -> Result<Option<CodecItem>, Error> {
        match self {
            Demuxer::Aac(d) => d.pull(),
            Demuxer::G711(d) => d.pull(),
            Demuxer::H264(d) => d.pull(),
            Demuxer::Onvif(d) => d.pull(),
        }
    }
}
