//! AAC (Advanced Audio Codec) decoding.
//! There are many intertwined standards; see the following references:
//! *   [RFC 3640](https://datatracker.ietf.org/doc/html/rfc3640): RTP Payload
//!     for Transport of MPEG-4 Elementary Streams.
//! *   ISO/IEC 13818-7: Advanced Audio Coding.
//! *   ISO/IEC 14496: Information technology — Coding of audio-visual objects
//!     *   ISO/IEC 14496-1: Systems.
//!     *   ISO/IEC 14496-3: Audio, subpart 1: Main.
//!     *   ISO/IEC 14496-3: Audio, subpart 4: General Audio coding (GA) — AAC, TwinVQ, BSAC.
//!     *   [ISO/IEC 14496-12](https://standards.iso.org/ittf/PubliclyAvailableStandards/c068960_ISO_IEC_14496-12_2015.zip):
//!         ISO base media file format.
//!     *   ISO/IEC 14496-14: MP4 File Format.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use failure::{Error, bail, format_err};
use pretty_hex::PrettyHex;
use std::{convert::TryFrom, fmt::Debug};

use crate::client::{DemuxedItem, rtp::Packet};

/// An AudioSpecificConfig as in ISO/IEC 14496-3 section 1.6.2.1.
/// Currently just a few fields of interest.
#[derive(Clone, Debug)]
struct AudioSpecificConfig {
    /// See ISO/IEC 14496-3 Table 1.3.
    audio_object_type: u8,
    frame_length: u32,
    sampling_frequency: u32,
    channels: &'static ChannelConfig,
}

/// A channel configuration as in ISO/IEC 14496-3 Table 1.19.
#[derive(Debug)]
struct ChannelConfig {
    channels: u16,

    /// The "number of considered channels" as defined in ISO/IEC 13818-7 Term
    /// 3.58. Roughly, non-subwoofer channels.
    ncc: u16,

    /// A human-friendly name for the channel configuration.
    name: &'static str,
}

#[rustfmt::skip]
const CHANNEL_CONFIGS: [Option<ChannelConfig>; 8] = [
    /* 0 */ None, // "defined in AOT related SpecificConfig"
    /* 1 */ Some(ChannelConfig { channels: 1, ncc: 1, name: "mono" }),
    /* 2 */ Some(ChannelConfig { channels: 2, ncc: 2, name: "stereo" }),
    /* 3 */ Some(ChannelConfig { channels: 3, ncc: 3, name: "3.0" }),
    /* 4 */ Some(ChannelConfig { channels: 4, ncc: 4, name: "4.0" }),
    /* 5 */ Some(ChannelConfig { channels: 5, ncc: 5, name: "5.0" }),
    /* 6 */ Some(ChannelConfig { channels: 6, ncc: 5, name: "5.1" }),
    /* 7 */ Some(ChannelConfig { channels: 8, ncc: 7, name: "7.1" }),
];

impl AudioSpecificConfig {
    /// Parses from raw bytes.
    fn parse(config: &[u8]) -> Result<Self, Error> {
        let mut r = bitreader::BitReader::new(config);
        let audio_object_type = match r.read_u8(5)? {
            31 => 32 + r.read_u8(6)?,
            o => o,
        };

        // ISO/IEC 14496-3 section 1.6.3.4.
        let sampling_frequency = match r.read_u8(4)? {
            0x0 => 96_000,
            0x1 => 88_200,
            0x2 => 64_000,
            0x3 => 48_000,
            0x5 => 32_000,
            0x6 => 24_000,
            0x7 => 22_050,
            0x8 => 16_000,
            0x9 => 12_000,
            0xa => 11_025,
            0xb =>  8_000,
            0xc =>  7_350,
            v @ 0xd | v @ 0xe => bail!("reserved sampling_frequency_index value 0x{:x}", v),
            0xf => r.read_u32(24)?,
            _ => unreachable!(),
        };
        let channels = {
            let c = r.read_u8(4)?;
            CHANNEL_CONFIGS.get(usize::from(c))
                .ok_or_else(|| format_err!("reserved channelConfiguration 0x{:x}", c))?
                .as_ref()
                .ok_or_else(|| format_err!("program_config_element parsing unimplemented"))?
        };
        if audio_object_type == 5 || audio_object_type == 29 {
            // extensionSamplingFrequencyIndex + extensionSamplingFrequency.
            if r.read_u8(4)? == 0xf {
                r.skip(24)?;
            }
            // audioObjectType (a different one) + extensionChannelConfiguration.
            if r.read_u8(5)? == 22 {
                r.skip(4)?;
            }
        }

        // The supported types here are the ones that use GASpecificConfig.
        match audio_object_type {
            1 | 2 | 3 | 4 | 6 | 7 | 17 | 19 | 20 | 21 | 22 | 23 => {},
            o => bail!("unsupported audio_object_type {}", o),
        }

        // GASpecificConfig, ISO/IEC 14496-3 section 4.4.1.
        let frame_length = match (audio_object_type, r.read_bool()?) {
            (3 /* AAC SR */, false) => 256,
            (3 /* AAC SR */, true) => bail!("frame_length_flag must be false for AAC SSR"),
            (23 /* ER AAC LD */, false) => 512,
            (23 /* ER AAC LD */, true) => 480,
            (_, false) => 1024,
            (_, true) => 960,
        };

        Ok(AudioSpecificConfig {
            audio_object_type,
            frame_length,
            sampling_frequency,
            channels,
        })
    }
}

/// Overwrites a buffer with a varint length, returning the length of the length.
/// See ISO/IEC 14496-1 section 8.3.3.
fn set_length(len: usize, data: &mut [u8]) -> Result<usize, Error> {
    if len < 1 << 7 {
        data[0] = len as u8;
        Ok(1)
    } else if len < 1 << 14 {
        data[0] = (( len        & 0x7F) | 0x80) as u8;
        data[1] =   (len >>  7)                 as u8;
        Ok(2)
    } else if len < 1 << 21 {
        data[0] = (( len        & 0x7F) | 0x80) as u8;
        data[1] = (((len >>  7) & 0x7F) | 0x80) as u8;
        data[2] =   (len >> 14)                 as u8;
        Ok(3)
    } else if len < 1 << 28 {
        data[0] = (( len        & 0x7F) | 0x80) as u8;
        data[1] = (((len >>  7) & 0x7F) | 0x80) as u8;
        data[2] = (((len >> 14) & 0x7F) | 0x80) as u8;
        data[3] =   (len >> 21)                 as u8;
        Ok(4)
    } else {
        // BaseDescriptor sets a maximum length of 2**28 - 1.
        bail!("length {} too long", len);
    }
}

/// Writes a box length and type (four-character code) for everything appended
/// in the supplied scope.
macro_rules! write_box {
    ($buf:expr, $fourcc:expr, $b:block) => {{
        let _: &mut BytesMut = $buf; // type-check.
        let pos_start = $buf.len();
        let fourcc: &[u8; 4] = $fourcc;
        $buf.extend_from_slice(&[0, 0, 0, 0, fourcc[0], fourcc[1], fourcc[2], fourcc[3]]);
        let r = {
            $b;
        };
        let pos_end = $buf.len();
        let len = pos_end.checked_sub(pos_start).unwrap();
        $buf[pos_start..pos_start+4].copy_from_slice(&u32::try_from(len)?.to_be_bytes()[..]);
        r
    }};
}

/// Writes a descriptor tag and length for everything appended in the supplied
/// scope. See ISO/IEC 14496-1 Table 1 for the `tag`.
macro_rules! write_descriptor {
    ($buf:expr, $tag:expr, $b:block) => {{
        let _: &mut BytesMut = $buf; // type-check.
        let _: u8 = $tag;
        let pos_start = $buf.len();

        // Overallocate room for the varint length and append the body.
        $buf.extend_from_slice(&[$tag, 0, 0, 0, 0]);
        let r = {
            $b;
        };
        let pos_end = $buf.len();

        // Then fix it afterward: write the correct varint length and move
        // the body backward. This approach seems better than requiring the
        // caller to first prepare the body in a separate allocation (and
        // awkward code ordering), or (as ffmpeg does) writing a "varint"
        // which is padded with leading 0x80 bytes.
        let len = pos_end.checked_sub(pos_start + 5).unwrap();
        let len_len = set_length(len, &mut $buf[pos_start+1..pos_start+4])?;
        $buf.copy_within(pos_start+5..pos_end, pos_start + 1 + len_len);
        $buf.truncate(pos_end + len_len - 4);
        r
    }};
}

/// Returns an MP4AudioSampleEntry (`mp4a`) box as in ISO/IEC 14496-14 section 5.6.1.
/// `config` should be a raw AudioSpecificConfig (matching `parsed`).
fn get_mp4a_box(parsed: &AudioSpecificConfig, config: &[u8]) -> Result<Bytes, Error> {
    let mut buf = BytesMut::new();

    // Write an MP4AudioSampleEntry (`mp4a`), as in ISO/IEC 14496-14 section 5.6.1.
    // It's based on AudioSampleEntry, ISO/IEC 14496-12 section 12.2.3.2,
    // in turn based on SampleEntry, ISO/IEC 14496-12 section 8.5.2.2.
    write_box!(&mut buf, b"mp4a", {
        buf.extend_from_slice(&[
            0, 0, 0, 0,             // SampleEntry.reserved
            0, 0, 0, 1,             // SampleEntry.reserved, SampleEntry.data_reference_index (1)
            0, 0, 0, 0,             // AudioSampleEntry.reserved
            0, 0, 0, 0,             // AudioSampleEntry.reserved
        ]);
        buf.put_u16(u16::from(parsed.channels.channels));
        buf.extend_from_slice(&[
            0x00, 0x10,             // AudioSampleEntry.samplesize
            0x00, 0x00, 0x00, 0x00, // AudioSampleEntry.pre_defined, AudioSampleEntry.reserved
        ]);

        // ISO/IEC 14496-12 section 12.2.3 says to put the samplerate (aka
        // clock_rate aka sampling_frequency) as a 16.16 fixed-point number or
        // use a SamplingRateBox. The latter also requires changing the
        // version/structure of the AudioSampleEntryBox and the version of the
        // stsd box. Just support the former for now.
        let sampling_frequency = u16::try_from(parsed.sampling_frequency)
            .map_err(|_| format_err!("aac sampling_frequency={} unsupported",
                                     parsed.sampling_frequency))?;
        buf.put_u32(u32::from(sampling_frequency) << 16);

        // Write the embedded ESDBox (`esds`), as in ISO/IEC 14496-14 section 5.6.1.
        write_box!(&mut buf, b"esds", {
            buf.put_u32(0); // version

            write_descriptor!(&mut buf, 0x03 /* ES_DescrTag */, {
                // The ESDBox contains an ES_Descriptor, defined in ISO/IEC 14496-1 section 8.3.3.
                // ISO/IEC 14496-14 section 3.1.2 has advice on how to set its
                // fields within the scope of a .mp4 file.
                buf.extend_from_slice(&[
                    0, 0, // ES_ID=0
                    0x00, // streamDependenceFlag, URL_Flag, OCRStreamFlag, streamPriority.
                ]);

                // DecoderConfigDescriptor, defined in ISO/IEC 14496-1 section 7.2.6.6.
                write_descriptor!(&mut buf, 0x04 /* DecoderConfigDescrTag */, {
                    buf.extend_from_slice(&[
                        0x40,    // objectTypeIndication = Audio ISO/IEC 14496-3
                        0x15,    // streamType = audio, upstream = false, reserved = 1
                    ]);

                    // bufferSizeDb is "the size of the decoding buffer for this
                    // elementary stream in byte". ISO/IEC 13818-7 section
                    // 8.2.2.1 defines the total decoder input buffer size as
                    // 6144 bits per NCC.
                    let buffer_size_bytes = (6144 / 8) * u32::from(parsed.channels.ncc);
                    if buffer_size_bytes > 0xFF_FFFF {
                        bail!("unreasonable buffer_size_bytes={}", buffer_size_bytes);
                    }

                    // buffer_size_bytes as a 24-bit number
                    buf.put_u8((buffer_size_bytes >> 16) as u8);
                    buf.put_u16(buffer_size_bytes as u16);

                    let max_bitrate = (6144 / 1024) * u32::from(parsed.channels.ncc)
                        * u32::from(sampling_frequency);
                    buf.put_u32(max_bitrate);

                    // avg_bitrate. ISO/IEC 14496-1 section 7.2.6.6 says "for streams with
                    // variable bitrate this value shall be set to zero."
                    buf.put_u32(0);

                    // AudioSpecificConfiguration, ISO/IEC 14496-3 subpart 1 section 1.6.2.
                    write_descriptor!(&mut buf, 0x05 /* DecSpecificInfoTag */, {
                        buf.extend_from_slice(config);
                    });
                });

                // SLConfigDescriptor, ISO/IEC 14496-1 section 7.3.2.3.1.
                write_descriptor!(&mut buf, 0x06 /* SLConfigDescrTag */, {
                    buf.put_u8(2); // predefined = reserved for use in MP4 files
                });
            });
        });
    });
    Ok(buf.freeze())
}

#[derive(Clone)]
pub struct Parameters {
    config: AudioSpecificConfig,
    rfc6381_codec: String,
    sample_entry: Bytes,
}

impl Parameters {
    /// Parses metadata from the `format-specific-params` of a SDP `fmtp` media attribute.
    /// The metadata is defined in [RFC 3640 section
    /// 4.1](https://datatracker.ietf.org/doc/html/rfc3640#section-4.1).
    pub fn from_format_specific_params(format_specific_params: &str) -> Result<Self, Error> {
        let mut mode = None;
        let mut config = None;
        let mut size_length = None;
        let mut index_length = None;
        let mut index_delta_length = None;
        for p in format_specific_params.split(';') {
            let p = p.trim();
            if p == "" {
                // Reolink cameras leave a trailing ';'.
                continue;
            }
            let (key, value) = crate::client::parse::split_once(p, '=')
                .ok_or_else(|| format_err!("bad format-specific-param {}", p))?;
            match &key.to_ascii_lowercase()[..] {
                "config" => {
                    config = Some(hex::decode(value)
                        .map_err(|_| format_err!("config has invalid hex encoding"))?);
                },
                "mode" => mode = Some(value),
                "sizelength" => {
                    size_length = Some(u16::from_str_radix(value, 10)
                        .map_err(|_| format_err!("bad sizeLength"))?);
                },
                "indexlength" => {
                    index_length = Some(u16::from_str_radix(value, 10)
                        .map_err(|_| format_err!("bad indexLength"))?);
                },
                "indexdeltalength" => {
                    index_delta_length = Some(u16::from_str_radix(value, 10)
                        .map_err(|_| format_err!("bad indexDeltaLength"))?);
                },
                _ => {},
            }
        }
        // https://datatracker.ietf.org/doc/html/rfc3640#section-3.3.6 AAC-hbr
        if mode != Some("AAC-hbr") {
            bail!("Expected mode AAC-hbr, got {:#?}", mode);
        }
        let config = config.ok_or_else(|| format_err!("config must be specified"))?;
        if size_length != Some(13) || index_length != Some(3) || index_delta_length != Some(3) {
            bail!("Unexpected sizeLength={:?} indexLength={:?} indexDeltaLength={:?}",
                  size_length, index_length, index_delta_length);
        }

        let parsed = AudioSpecificConfig::parse(&config[..])?;
        let sample_entry = get_mp4a_box(&parsed, &config[..])?;

        // https://datatracker.ietf.org/doc/html/rfc6381#section-3.3
        let rfc6381_codec = format!("mp4a.40.{}", parsed.audio_object_type);
        Ok(Parameters {
            config: parsed,
            rfc6381_codec,
            sample_entry,
        })
    }

    pub fn sampling_frequency(&self) -> u32 {
        self.config.sampling_frequency
    }

    pub fn sample_entry(&self) -> &[u8] {
        &self.sample_entry
    }
}

impl std::fmt::Debug for Parameters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("aac::Parameters")
         .field("config", &self.config)
         .field("rfc6381_codec", &self.rfc6381_codec)
         .field("sample_entry", &self.sample_entry.hex_dump())
         .finish()
    }
}

pub struct Frame {
    pub ctx: crate::Context,
    pub stream_id: usize,
    pub timestamp: crate::Timestamp,
    pub data: Bytes,
}

impl Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("aac::Frame")
         .field("stream_id", &self.stream_id)
         .field("timestamp", &self.timestamp)
         .field("data", &self.data.hex_dump())
         .finish()
    }
}

pub(crate) struct Demuxer {
    params: Parameters,
    state: DemuxerState,
}

struct Aggregate {
    ctx: crate::Context,

    stream_id: usize,

    /// The RTP-level timestamp; frame `i` is at timestamp `timestamp + frame_length*i`.
    timestamp: crate::Timestamp,

    /// The buffer, positioned at frame 0's header.
    buf: Bytes,

    /// The index in range `[0, frame_count)` of the next frame to output.
    frame_i: u16,

    /// The non-zero total frames within this aggregate.
    frame_count: u16,

    /// The starting byte offset of `frame_i`'s data within `buf`.
    data_off: usize,

    /// If a mark was set on this packet. When this is false, this should
    /// actually be the start of a fragmented frame, but that conversion is
    /// currently deferred until `pull`.
    mark: bool,
}

struct Fragment {
    rtp_timestamp: u16,
    size: u16,
    buf: BytesMut,
}

enum DemuxerState {
    Idle,
    Aggregated(Aggregate),
    Fragmented(Fragment),
    Ready(Frame),
}

impl Demuxer {
    pub(crate) fn new(params: Parameters) -> Box<dyn crate::client::Demuxer> {
        Box::new(Self {
            params,
            state: DemuxerState::Idle,
        })
    }
}

impl crate::client::Demuxer for Demuxer {
    fn push(&mut self, mut pkt: Packet) -> Result<(), Error> {
        // Read the AU headers.
        if pkt.payload.len() < 2 {
            bail!("packet too short for au-header-length");
        }
        let au_headers_length_bits = pkt.payload.get_u16();

        // AAC-hbr requires 16-bit AU headers: 13-bit size, 3-bit index.
        if (au_headers_length_bits & 0x7) != 0 {
            bail!("bad au-headers-length {}", au_headers_length_bits);
        }
        let au_headers_count = au_headers_length_bits >> 4;
        let data_off = usize::from(au_headers_count) << 1;
        if pkt.payload.len() < (usize::from(au_headers_count) << 1) {
            bail!("packet too short for au-headers");
        }
        match &mut self.state {
            DemuxerState::Fragmented(ref mut frag) => {
                if au_headers_count != 1 {
                    bail!("Got {}-AU packet while fragment in progress", au_headers_count);
                }
                if (pkt.timestamp.timestamp as u16) != frag.rtp_timestamp {
                    bail!("Timestamp changed from 0x{:04x} to 0x{:04x} mid-fragment",
                        frag.rtp_timestamp, pkt.timestamp.timestamp as u16);
                }
                let au_header = u16::from_be_bytes([pkt.payload[0], pkt.payload[1]]);
                let size = usize::from(au_header >> 3);
                if size != usize::from(frag.size) {
                    bail!("size changed {}->{} mid-fragment", frag.size, size);
                }
                let data = &pkt.payload[data_off..];
                match (frag.buf.len() + data.len()).cmp(&size) {
                    std::cmp::Ordering::Less => {
                        if pkt.mark {
                            bail!("frag marked complete when {}+{}<{}", frag.buf.len(), data.len(), size);
                        }
                    },
                    std::cmp::Ordering::Equal => {
                        if !pkt.mark {
                            bail!("frag not marked complete when full data present");
                        }
                        frag.buf.extend_from_slice(data);
                        println!("au {}: len-{}, fragmented", &pkt.timestamp, size);
                        self.state = DemuxerState::Ready(Frame {
                            ctx: pkt.rtsp_ctx,
                            stream_id: pkt.stream_id,
                            timestamp: pkt.timestamp,
                            data: std::mem::take(&mut frag.buf).freeze(),
                        });
                    },
                    std::cmp::Ordering::Greater => bail!("too much data in fragment"),
                }
            },
            DemuxerState::Aggregated(_) => panic!("push when already in state aggregated"),
            DemuxerState::Idle => {
                if au_headers_count == 0 {
                    bail!("aggregate with no headers");
                }
                self.state = DemuxerState::Aggregated(Aggregate {
                    ctx: pkt.rtsp_ctx,
                    stream_id: pkt.stream_id,
                    timestamp: pkt.timestamp,
                    buf: pkt.payload,
                    frame_i: 0,
                    frame_count: au_headers_count,
                    data_off,
                    mark: pkt.mark,
                });
            },
            DemuxerState::Ready(..) => panic!("push when in state ready"),
        }
        Ok(())
    }

    fn pull(&mut self) -> Result<Option<crate::client::DemuxedItem>, Error> {
        match std::mem::replace(&mut self.state, DemuxerState::Idle) {
            s @ DemuxerState::Idle | s @ DemuxerState::Fragmented(..) => {
                self.state = s;
                Ok(None)
            },
            DemuxerState::Ready(f) => {
                self.state = DemuxerState::Idle;
                Ok(Some(DemuxedItem::AudioFrame(f)))
            }
            DemuxerState::Aggregated(mut agg) => {
                let i = usize::from(agg.frame_i);
                let au_header = u16::from_be_bytes([agg.buf[i << 1], agg.buf[(i << 1) + 1]]);
                let size = usize::from(au_header >> 3);
                let index = au_header & 0b111;
                if index != 0 {
                    // First AU's index must be zero; subsequent AU's deltas > 1
                    // indicate interleaving, which we don't support.
                    // TODO: https://datatracker.ietf.org/doc/html/rfc3640#section-3.3.6
                    // says "receivers MUST support de-interleaving".
                    bail!("interleaving not yet supported");
                }
                if size > agg.buf.len() - agg.data_off { // start of fragment
                    if agg.frame_count != 1 {
                        bail!("fragmented AUs must not share packets");
                    }
                    if agg.mark {
                        bail!("mark can't be set on beginning of fragment");
                    }
                    let mut buf = BytesMut::with_capacity(size);
                    buf.extend_from_slice(&agg.buf[agg.data_off..]);
                    self.state = DemuxerState::Fragmented(Fragment {
                        rtp_timestamp: agg.timestamp.timestamp as u16,
                        size: size as u16,
                        buf,
                    });
                    return Ok(None);
                }
                if !agg.mark {
                    bail!("mark must be set on non-fragmented au");
                }
                let frame = Frame {
                    ctx: agg.ctx,
                    stream_id: agg.stream_id,
                    timestamp: agg.timestamp.try_add(u64::from(agg.frame_i) * u64::from(self.params.config.frame_length))?,
                    data: agg.buf.slice(agg.data_off..agg.data_off+size),
                };
                agg.data_off += size;
                agg.frame_i += 1;
                if agg.frame_i < agg.frame_count {
                    self.state = DemuxerState::Aggregated(agg);
                }
                Ok(Some(DemuxedItem::AudioFrame(frame)))
            },
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parse_audio_specific_config() {
        let dahua = super::AudioSpecificConfig::parse(&[0x11, 0x88]).unwrap();
        assert_eq!(dahua.sampling_frequency, 48_000);
        assert_eq!(dahua.channels.name, "mono");

        let bunny = super::AudioSpecificConfig::parse(&[0x14, 0x90]).unwrap();
        assert_eq!(bunny.sampling_frequency, 12_000);
        assert_eq!(bunny.channels.name, "stereo");

        let rfc3640 = super::AudioSpecificConfig::parse(&[0x11, 0xB0]).unwrap();
        assert_eq!(rfc3640.sampling_frequency, 48_000);
        assert_eq!(rfc3640.channels.name, "5.1");
    }
}
