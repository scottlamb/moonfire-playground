//! Proof-of-concept `.mp4` writer.
//!
//! This writes media data (`mdat`) to a stream, buffering parameters for a
//! `moov` atom at the end. This avoids the need to buffer the media data
//! (`mdat`) first or reserved a fixed size for the `moov`, but it will slow
//! playback, particularly when serving `.mp4` files remotely.
//! 
//! For a more high-quality implementation, see [Moonfire NVR](https://github.com/scottlamb/moonfire-nvr).
//! It's better tested, places the `moov` atom at the start, can do HTTP range
//! serving for arbitrary time ranges, and supports standard and fragmented
//! `.mp4` files.
//!
//! See the BMFF spec, ISO/IEC 14496-12:2015:
//! https://github.com/scottlamb/moonfire-nvr/wiki/Standards-and-specifications
//! https://standards.iso.org/ittf/PubliclyAvailableStandards/c068960_ISO_IEC_14496-12_2015.zip

use bytes::{Buf, BufMut, BytesMut};
use failure::{Error, bail, format_err};
use futures::StreamExt;
use log::info;
use moonfire_rtsp::client::video::Parameters as _;
use moonfire_rtsp::client::{DemuxedItem, audio::aac, video::h264};

use std::convert::TryFrom;
use std::io::SeekFrom;
use std::path::PathBuf;
use tokio::io::{AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use url::Url;

/// Writes a box length for everything appended in the supplied scope.
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

async fn write_all_buf<W: AsyncWrite + Unpin, B: Buf>(writer: &mut W, buf: &mut B) -> Result<(), Error> {
    // TODO: this doesn't use vectored I/O. Annoying.
    while buf.has_remaining() {
        writer.write_buf(buf).await?;
    }
    Ok(())
}

/// Writes `.mp4` data to a sink.
/// See module-level documentation for details.
pub struct Mp4Writer<W: AsyncWrite + AsyncSeek + Send + Unpin> {
    mdat_start: u32,
    mdat_pos: u32,
    video_params: Option<h264::Parameters>,
    audio_params: Option<aac::Parameters>,

    /// The (1-indexed) video sample (frame) number of each sync sample (random access point).
    video_sync_sample_nums: Vec<u32>,

    video_trak: TrakTracker,
    audio_trak: TrakTracker,
    inner: W,
}

/// Tracks the parts of a `trak` atom which are common between video and audio samples.
#[derive(Default)]
struct TrakTracker {
    samples: u32,
    next_pos: Option<u32>,
    chunks: Vec<(u32, u32)>,  // (1-based sample_number, byte_pos)
    sizes: Vec<u32>,

    /// The durations of samples in a run-length encoding form: (number of samples, duration).
    /// This lags one sample behind calls to `add_sample` because each sample's duration
    /// is calculated using the PTS of the following sample.
    durations: Vec<(u32, u32)>,
    last_pts: Option<u64>,
    tot_duration: u64,
}

impl TrakTracker {
    fn add_sample(&mut self, pos: u32, size: u32, timestamp: moonfire_rtsp::Timestamp) -> Result<(), Error> {
        self.samples += 1;
        if self.next_pos != Some(pos) {
            self.chunks.push((self.samples, pos));
        }
        self.sizes.push(size);
        self.next_pos = Some(pos + size);
        if let Some(last_pts) = self.last_pts.replace(timestamp.timestamp()) {
            let duration = timestamp.timestamp().checked_sub(last_pts).unwrap();
            assert!(duration > 0);
            self.tot_duration += duration;
            let duration = u32::try_from(duration)?;
            match self.durations.last_mut() {
                Some((s, d)) if *d == duration => *s += 1,
                _ => self.durations.push((1, duration)),
            }
        }
        Ok(())
    }

    fn finish(&mut self) {
        if self.last_pts.is_some() {
            self.durations.push((1, 0));
        }
    }

    /// Estimates the sum of the variable-sized portions of the data.
    fn size_estimate(&self) -> usize {
        (self.durations.len() * 8) + // stts
        (self.chunks.len() * 12) +   // stsc
        (self.sizes.len() * 4) +     // stsz
        (self.chunks.len() * 4)      // stco
    }

    fn write_common_stbl_parts(&self, buf: &mut BytesMut) -> Result<(), Error> {
        // TODO: add an edit list so the video and audio tracks are in sync.
        write_box!(buf, b"stts", {
            buf.put_u32(0);
            buf.put_u32(u32::try_from(self.durations.len())?);
            for (samples, duration) in &self.durations {
                buf.put_u32(*samples);
                buf.put_u32(*duration);
            }
        });
        write_box!(buf, b"stsc", {
            buf.put_u32(0); // version
            buf.put_u32(u32::try_from(self.chunks.len())?);
            let mut prev_sample_number = 1;
            let mut chunk_number = 1;
            for &(sample_number, _pos) in &self.chunks[1..] {
                buf.put_u32(chunk_number);
                buf.put_u32(sample_number - prev_sample_number);
                buf.put_u32(1); // sample_description_index
                prev_sample_number = sample_number;
                chunk_number += 1;
            }
            if !self.chunks.is_empty() {
                buf.put_u32(chunk_number);
                buf.put_u32(self.samples + 1 - prev_sample_number);
                buf.put_u32(1); // sample_description_index
            }
        });
        write_box!(buf, b"stsz", {
            buf.put_u32(0); // version
            buf.put_u32(0); // sample_size
            buf.put_u32(u32::try_from(self.sizes.len())?);
            for s in &self.sizes {
                buf.put_u32(*s);
            }
        });
        write_box!(buf, b"stco", {
            buf.put_u32(0); // version
            buf.put_u32(u32::try_from(self.chunks.len())?); // entry_count
            for &(_sample_number, pos) in &self.chunks {
                buf.put_u32(pos);
            }
        });
        Ok(())
    }
}

impl<W: AsyncWrite + AsyncSeek + Send + Unpin> Mp4Writer<W> {
    pub async fn new(video_params: Option<h264::Parameters>, audio_params: Option<aac::Parameters>,
                     mut inner: W) -> Result<Self, Error> {
        let mut buf = BytesMut::new();
        write_box!(&mut buf, b"ftyp", {
            buf.extend_from_slice(&[
                b'i', b's', b'o', b'm', // major_brand
                0, 0, 0, 0,             // minor_version
                b'i', b's', b'o', b'm', // compatible_brands[0]
            ]);
        });
        buf.extend_from_slice(&b"\0\0\0\0mdat"[..]);
        let mdat_start = u32::try_from(buf.len())?;
        write_all_buf(&mut inner, &mut buf).await?;
        Ok(Mp4Writer {
            inner,
            video_params,
            audio_params,
            video_trak: TrakTracker::default(),
            audio_trak: TrakTracker::default(),
            video_sync_sample_nums: Vec::new(),
            mdat_start,
            mdat_pos: mdat_start,
        })
    }

    pub async fn finish(mut self) -> Result<(), Error> {
        self.video_trak.finish();
        self.audio_trak.finish();
        let mut buf = BytesMut::with_capacity(
            1024 + self.video_trak.size_estimate() + self.audio_trak.size_estimate() +
            4*self.video_sync_sample_nums.len());
        write_box!(&mut buf, b"moov", {
            write_box!(&mut buf, b"mvhd", {
                buf.put_u32(1 << 24);           // version
                buf.put_u64(0);                 // creation_time
                buf.put_u64(0);                 // modification_time
                buf.put_u32(90000);             // timescale
                buf.put_u64(self.video_trak.tot_duration);
                buf.put_u32(0x00010000);        // rate
                buf.put_u16(0x0100);            // volume
                buf.put_u16(0);                 // reserved
                buf.put_u64(0);                 // reserved
                for v in &[0x00010000,0,0,0,0x00010000,0,0,0,0x40000000] {
                    buf.put_u32(*v);            // matrix
                }
                for _ in 0..6 {
                    buf.put_u32(0);             // pre_defined
                }
                buf.put_u32(2);                 // next_track_id
            });
            if let Some(p) = self.video_params.as_ref() {
                self.write_video_trak(&mut buf, p)?;
            }
            if let Some(p) = self.audio_params.as_ref() {
                self.write_audio_trak(&mut buf, p)?;
            }
        });
        write_all_buf(&mut self.inner, &mut buf.freeze()).await?;
        self.inner.seek(SeekFrom::Start(u64::from(self.mdat_start - 8))).await?;
        self.inner.write_all(&u32::try_from(self.mdat_pos + 8 - self.mdat_start)?.to_be_bytes()[..]).await?;
        Ok(())
    }

    fn write_video_trak(&self, buf: &mut BytesMut, parameters: &h264::Parameters) -> Result<(), Error> {
        write_box!(buf, b"trak", {
            write_box!(buf, b"tkhd", {
                buf.put_u32((1 << 24) | 7); // version, flags
                buf.put_u64(0);             // creation_time
                buf.put_u64(0);             // modification_time
                buf.put_u32(1);             // track_id
                buf.put_u32(0);             // reserved
                buf.put_u64(self.video_trak.tot_duration);
                buf.put_u64(0);             // reserved
                buf.put_u16(0);             // layer
                buf.put_u16(0);             // alternate_group
                buf.put_u16(0);             // volume
                buf.put_u16(0);             // reserved
                for v in &[0x00010000,0,0,0,0x00010000,0,0,0,0x40000000] {
                    buf.put_u32(*v);        // matrix
                }
                let dims = self.video_params.as_ref().map(h264::Parameters::pixel_dimensions).unwrap_or((0, 0));
                let width = u32::from(u16::try_from(dims.0)?) << 16;
                let height = u32::from(u16::try_from(dims.1)?) << 16;
                buf.put_u32(width);
                buf.put_u32(height);
            });
            write_box!(buf, b"mdia", {
                write_box!(buf, b"mdhd", {
                    buf.put_u32(1 << 24);       // version
                    buf.put_u64(0);             // creation_time
                    buf.put_u64(0);             // modification_time
                    buf.put_u32(90000);         // timebase
                    buf.put_u64(self.video_trak.tot_duration);
                    buf.put_u32(0x55c40000);    // language=und + pre-defined
                });
                write_box!(buf, b"hdlr", {
                    buf.extend_from_slice(&[
                        0x00, 0x00, 0x00, 0x00, // version + flags
                        0x00, 0x00, 0x00, 0x00, // pre_defined
                        b'v', b'i', b'd', b'e', // handler = vide
                        0x00, 0x00, 0x00, 0x00, // reserved[0]
                        0x00, 0x00, 0x00, 0x00, // reserved[1]
                        0x00, 0x00, 0x00, 0x00, // reserved[2]
                        0x00, // name, zero-terminated (empty)
                    ]);
                });
                write_box!(buf, b"minf", {
                    write_box!(buf, b"vmhd", {
                        buf.put_u32(1);
                        buf.put_u64(0);
                    });
                    write_box!(buf, b"dinf", {
                        write_box!(buf, b"dref", {
                            buf.put_u32(0);
                            buf.put_u32(1); // entry_count
                            write_box!(buf, b"url ", {
                                buf.put_u32(1); // version, flags=self-contained
                            });
                        });
                    });
                    write_box!(buf, b"stbl", {
                        write_box!(buf, b"stsd", {
                            buf.put_u32(0); // version
                            buf.put_u32(1); // entry_count
                            self.write_video_sample_entry(buf, parameters)?;
                        });
                        self.video_trak.write_common_stbl_parts(buf)?;
                        write_box!(buf, b"stss", {
                            buf.put_u32(0); // version
                            buf.put_u32(u32::try_from(self.video_sync_sample_nums.len())?);
                            for n in &self.video_sync_sample_nums {
                                buf.put_u32(*n);
                            }
                        });
                    });
                });
            });
        });
        Ok(())
    }

    fn write_audio_trak(&self, buf: &mut BytesMut, parameters: &aac::Parameters) -> Result<(), Error> {
        write_box!(buf, b"trak", {
            write_box!(buf, b"tkhd", {
                buf.put_u32((1 << 24) | 7); // version, flags
                buf.put_u64(0);             // creation_time
                buf.put_u64(0);             // modification_time
                buf.put_u32(2);             // track_id
                buf.put_u32(0);             // reserved
                buf.put_u64(self.audio_trak.tot_duration);
                buf.put_u64(0);             // reserved
                buf.put_u16(0);             // layer
                buf.put_u16(0);             // alternate_group
                buf.put_u16(0);             // volume
                buf.put_u16(0);             // reserved
                for v in &[0x00010000,0,0,0,0x00010000,0,0,0,0x40000000] {
                    buf.put_u32(*v);        // matrix
                }
                buf.put_u32(0); // width
                buf.put_u32(0); // height
            });
            write_box!(buf, b"mdia", {
                write_box!(buf, b"mdhd", {
                    buf.put_u32(1 << 24);       // version
                    buf.put_u64(0);             // creation_time
                    buf.put_u64(0);             // modification_time
                    buf.put_u32(parameters.sampling_frequency());
                    buf.put_u64(self.audio_trak.tot_duration);
                    buf.put_u32(0x55c40000);    // language=und + pre-defined
                });
                write_box!(buf, b"hdlr", {
                    buf.extend_from_slice(&[
                        0x00, 0x00, 0x00, 0x00, // version + flags
                        0x00, 0x00, 0x00, 0x00, // pre_defined
                        b's', b'o', b'u', b'n', // handler = soun
                        0x00, 0x00, 0x00, 0x00, // reserved[0]
                        0x00, 0x00, 0x00, 0x00, // reserved[1]
                        0x00, 0x00, 0x00, 0x00, // reserved[2]
                        0x00, // name, zero-terminated (empty)
                    ]);
                });
                write_box!(buf, b"minf", {
                    write_box!(buf, b"smhd", {
                        buf.extend_from_slice(&[
                            0x00, 0x00, 0x00, 0x00, // version + flags
                            0x00, 0x00,             // balance
                            0x00, 0x00,             // reserved
                        ]);
                    });
                    write_box!(buf, b"dinf", {
                        write_box!(buf, b"dref", {
                            buf.put_u32(0);
                            buf.put_u32(1); // entry_count
                            write_box!(buf, b"url ", {
                                buf.put_u32(1); // version, flags=self-contained
                            });
                        });
                    });
                    write_box!(buf, b"stbl", {
                        write_box!(buf, b"stsd", {
                            buf.put_u32(0); // version
                            buf.put_u32(1); // entry_count
                            buf.extend_from_slice(parameters.sample_entry());
                        });
                        self.audio_trak.write_common_stbl_parts(buf)?;

                        // AAC requires two samples (really, each is a set of 960 or 1024 samples)
                        // to decode accurately. See
                        // https://developer.apple.com/library/archive/documentation/QuickTime/QTFF/QTFFAppenG/QTFFAppenG.html .
                        write_box!(buf, b"sgpd", { // BMFF section 8.9.3: SampleGroupDescriptionBox
                            buf.put_u32(0); // version
                            buf.extend_from_slice(b"roll"); // grouping type
                            buf.put_u32(1); // entry_count
                            // BMFF section 10.1: AudioRollRecoveryEntry
                            buf.put_i16(-1); // roll_distance
                        });
                        write_box!(buf, b"sbgp", { // BMFF section 8.9.2: SampleToGroupBox
                            buf.put_u32(0); // version
                            buf.extend_from_slice(b"roll"); // grouping type
                            buf.put_u32(1); // entry_count
                            buf.put_u32(self.audio_trak.samples);
                            buf.put_u32(1); // group_description_index
                        });
                    });
                });
            });
        });
        Ok(())
    }

    fn write_video_sample_entry(&self, buf: &mut BytesMut, parameters: &h264::Parameters) -> Result<(), Error> {
        write_box!(buf, b"avc1", {
            buf.put_u32(0);
            buf.put_u32(1); // data_reference_index = 1
            buf.extend_from_slice(&[0; 16]);
            buf.put_u16(u16::try_from(parameters.pixel_dimensions().0)?);
            buf.put_u16(u16::try_from(parameters.pixel_dimensions().1)?);
            buf.extend_from_slice(&[
                0x00, 0x48, 0x00, 0x00, // horizresolution
                0x00, 0x48, 0x00, 0x00, // vertresolution
                0x00, 0x00, 0x00, 0x00, // reserved
                0x00, 0x01, // frame count
                0x00, 0x00, 0x00, 0x00, // compressorname
                0x00, 0x00, 0x00, 0x00, //
                0x00, 0x00, 0x00, 0x00, //
                0x00, 0x00, 0x00, 0x00, //
                0x00, 0x00, 0x00, 0x00, //
                0x00, 0x00, 0x00, 0x00, //
                0x00, 0x00, 0x00, 0x00, //
                0x00, 0x00, 0x00, 0x00, //
                0x00, 0x18, 0xff, 0xff, // depth + pre_defined
            ]);
            write_box!(buf, b"avcC", {
                buf.extend_from_slice(parameters.avc_decoder_config());
            });
        });
        Ok(())
    }

    async fn parameters_change(&mut self, parameters: h264::Parameters) -> Result<(), failure::Error> {
        bail!("parameters change unimplemented. new parameters: {:#?}", parameters)
    }

    async fn picture(&mut self, mut picture: moonfire_rtsp::client::video::Picture) -> Result<(), failure::Error> {
        println!("{}: {}-byte picture", &picture.timestamp, picture.remaining());
        let size = u32::try_from(picture.remaining())?;
        self.video_trak.add_sample(self.mdat_pos, size, picture.timestamp)?;
        self.mdat_pos = self.mdat_pos.checked_add(size).ok_or_else(|| format_err!("mdat_pos overflow"))?;
        if picture.is_random_access_point {
            self.video_sync_sample_nums.push(u32::try_from(self.video_trak.samples)?);
        }
        write_all_buf(&mut self.inner, &mut picture).await?;
        Ok(())
    }

    async fn audio_frame(&mut self, mut frame: moonfire_rtsp::client::audio::aac::Frame) -> Result<(), failure::Error> {
        println!("{}: {}-byte audio frame", &frame.timestamp, frame.data.remaining());
        let size = u32::try_from(frame.data.remaining())?;
        self.audio_trak.add_sample(self.mdat_pos, size, frame.timestamp)?;
        self.mdat_pos = self.mdat_pos.checked_add(size).ok_or_else(|| format_err!("mdat_pos overflow"))?;
        write_all_buf(&mut self.inner, &mut frame.data).await?;
        Ok(())
    }
}

pub async fn run(url: Url, credentials: Option<moonfire_rtsp::client::Credentials>,
                 no_video: bool, no_audio: bool, out: PathBuf) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c();
    let mut session = moonfire_rtsp::client::Session::describe(url, credentials).await?;
    let video_stream_i = if no_video {
        None
    } else {
        session.streams().iter().position(|s| s.media == "video" && s.parameters.is_some())
    };
    let video_parameters = if let Some(i) = video_stream_i {
        session.setup(i).await?;
        let params = match session.streams()[i].parameters.as_ref() {
            Some(moonfire_rtsp::client::Parameters::H264(h264)) => h264.clone(),
            _ => panic!(),
        };
        info!("video parameters: {:#?}", &params);
        Some(params)
    } else {
        None
    };
    let audio_stream_i = if no_audio {
        None
    } else {
        session.streams().iter().position(|s| s.media == "audio" && s.parameters.is_some())
    };
    let audio_parameters = if let Some(i) = audio_stream_i {
        session.setup(i).await?;
        let params = match session.streams()[i].parameters.as_ref() {
            Some(moonfire_rtsp::client::Parameters::Aac(aac)) => aac.clone(),
            _ => panic!(),
        };
        info!("audio parameters: {:#?}", &params);
        Some(params)
    } else {
        None
    };
    let session = session.play().await?.demuxed()?;

    // Read RTP data.
    let out = tokio::fs::File::create(out).await?;
    let mut mp4 = Mp4Writer::new(video_parameters.clone(), audio_parameters.clone(), out).await?;

    tokio::pin!(session);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            pkt = session.next() => {
                match pkt.ok_or_else(|| format_err!("EOF"))?? {
                    DemuxedItem::Picture(p) => mp4.picture(p).await?,
                    DemuxedItem::AudioFrame(f) => mp4.audio_frame(f).await?,
                    DemuxedItem::ParameterChange(p) => mp4.parameters_change(p).await?,
                    _ => continue,
                };
            },
            _ = &mut stop => {
                break;
            },
        }
    }
    info!("Stopping");
    mp4.finish().await?;
    Ok(())
}
