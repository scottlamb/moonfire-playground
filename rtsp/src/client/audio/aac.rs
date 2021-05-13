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

use bytes::{BufMut, Bytes, BytesMut};
use failure::{Error, bail, format_err};
use pretty_hex::PrettyHex;
use std::convert::TryFrom;

/// An AudioSpecificConfig as in ISO/IEC 14496-3 section 1.6.2.1.
/// Currently just a few fields of interest.
#[derive(Debug)]
struct AudioSpecificConfig {
    /// See ISO/IEC 14496-3 Table 1.3.
    audio_object_type: u8,
    sampling_frequency: u32,
    channels: u8,
}

impl AudioSpecificConfig {
    /// Parses from raw bytes.
    fn parse(config: &[u8]) -> Result<Self, Error> {
        let mut r = bitreader::BitReader::new(config);
        let audio_object_type = match r.read_u8(5)? {
            31 => 32 + r.read_u8(6)?,
            o => o,
        };

        // 1.6.3.4.
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
        let channels = match r.read_u8(4).unwrap() {
            0 => bail!("interpreting AOT related SpecificConfig unimplemented"),
            i @ 1..=7 => i,
            v @ 8..=15 => bail!("reserved channelConfiguration value 0x{:x}", v),
            _ => unreachable!(),
        };
        Ok(AudioSpecificConfig {
            audio_object_type,
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

/// Writes a descriptor tag and length for everything appended in the supplie
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
        buf.put_u16(u16::from(parsed.channels));
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
                    write_descriptor!(&mut buf, 0x05 /* DecSpecificInfoTag */, {
                        buf.extend_from_slice(config);
                    });

                    // bufferSizeDb is "the size of the decoding buffer for this
                    // elementary stream in byte". ISO/IEC 13818-7 section
                    // 8.2.2.1 defines the total decoder input buffer size as
                    // 6144 bits per [channel]. There are exceptions for a "low
                    // sampling frequency enhancement channel" (aka the .1 in
                    // 5.1) and "dependent coupling channel".
                    // TODO: get those right. Security cameras tend to be mono
                    // anyway though.
                    let buffer_size_bytes = (6144 / 8) * u32::from(parsed.channels);
                    if buffer_size_bytes > 0xFF_FFFF {
                        bail!("unreasonable buffer_size_bytes={}", buffer_size_bytes);
                    }

                    // buffer_size_bytes as a 24-bit number
                    buf.put_u8((buffer_size_bytes >> 16) as u8);
                    buf.put_u16(buffer_size_bytes as u16);

                    let max_bitrate = (6144 / 1024) * u32::from(parsed.channels)
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

pub struct Parameters {
    config: AudioSpecificConfig,
    rfc6381_codec: String,
    sample_entry: Bytes,
}

impl Parameters {
    /// Parses metadata from the `format-specific-params` of a SDP `fmtp` media attribute.
    pub fn from_format_specific_params(format_specific_params: &str) -> Result<Self, Error> {
        let mut mode = None;
        let mut config = None;
        for p in format_specific_params.split(';') {
            if let Some(c) = p.strip_prefix("config=") {
                config = Some(hex::decode(c)
                    .map_err(|_| format_err!("config has invalid hex encoding"))?);
            } else if let Some(m) = p.strip_prefix("mode=") {
                mode = Some(m);
            }
        }
        // https://datatracker.ietf.org/doc/html/rfc3640#section-3.3.5 AAC-lbr
        // https://datatracker.ietf.org/doc/html/rfc3640#section-3.3.6 AAC-hbr
        if !matches!(mode, Some(m) if m == "AAC-hbr" || m == "AAC-lbr") {
            bail!("Expected mode AAC-hbr or AAC-lbr, got {:#?}", mode);
        }
        let config = config.ok_or_else(|| format_err!("expected config in format-specific-params"))?;
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

#[cfg(test)]
mod tests {
    #[test]
    fn parse_audio_specific_config() {
        let dahua = super::AudioSpecificConfig::parse(&[0x11, 0x88]).unwrap();
        assert_eq!(dahua.sampling_frequency, 48_000);
        assert_eq!(dahua.channels, 1);

        let bunny = super::AudioSpecificConfig::parse(&[0x14, 0x90]).unwrap();
        assert_eq!(bunny.sampling_frequency, 12_000);
        assert_eq!(bunny.channels, 2);
    }
}
