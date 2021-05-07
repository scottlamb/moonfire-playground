//! Proof-of-concept `.mp4` writer.
//!
//! This writes media data (`mdat`) to a stream, buffering metadata for a
//! `moov` atom at the end. This avoids the need to buffer the media data
//! (`mdat`) first or reserved a fixed size for the `moov`, but it will slow
//! playback, particularly when serving `.mp4` files remotely.
//! 
//! For a more high-quality implementation, see [Moonfire NVR](https://github.com/scottlamb/moonfire-nvr).
//! It's better tested, places the `moov` atom at the start, can do HTTP range
//! serving for arbitrary time ranges, and supports standard and fragmented
//! `.mp4` files.

use async_trait::async_trait;
use bytes::{Buf, BufMut, BytesMut};
use failure::{Error, bail, format_err};
use log::{info, trace};
use moonfire_rtsp::client::{ChannelType, video::{Metadata, VideoHandler, h264}};
use moonfire_rtsp::client::ChannelHandler;
use std::convert::TryFrom;
use std::io::SeekFrom;
use std::path::PathBuf;
use tokio::io::{AsyncSeek, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use url::Url;

/// Writes a box length for everything appended in the supplied scope.
/// Used only within FileBuilder::build (and methods it calls internally).
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

/// Writes `.mp4` data to a sink.
/// See module-level documentation for details.
pub struct Mp4Writer<W: AsyncWrite + AsyncSeek + Send + Unpin> {
    mdat_start: u32,
    mdat_len: u32,
    metadata: moonfire_rtsp::client::video::h264::Metadata,
    last_pts: Option<u64>,

    /// Differences between pairs of pts, in timescale units.
    /// Used for the `stts` box. Lags one behind writing.
    durations: Vec<u32>,

    /// Byte sizes of all written samples.
    sizes: Vec<u32>,

    /// The (1-indexed!) frame numbers of each sync sample (random access
    /// point).
    sync_sample_nums: Vec<u32>,

    tot_duration: u64,

    inner: W,
}

impl<W: AsyncWrite + AsyncSeek + Send + Unpin> Mp4Writer<W> {
    pub async fn new(metadata: moonfire_rtsp::client::video::h264::Metadata, mut inner: W) -> Result<Self, Error> {
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
        inner.write_buf(&mut buf).await?;
        Ok(Mp4Writer {
            inner,
            metadata,
            last_pts: None,
            durations: Vec::new(),
            tot_duration: 0,
            sizes: Vec::new(),
            sync_sample_nums: Vec::new(),
            mdat_start,
            mdat_len: 8,
        })
    }

    /*/// Returns the total duration, as clock ticks and clock rate (Hz).
    pub fn duration(&self) -> (u64, u32) {
        (self.tot_duration, 90_000)
    }*/

    pub async fn finish(mut self) -> Result<(), Error> {
        if self.last_pts.is_some() {
            self.durations.push(0u32);
        }
        let mut buf = BytesMut::with_capacity(1024 + 8*self.sizes.len() + 4*self.sync_sample_nums.len());
        write_box!(&mut buf, b"moov", {
            write_box!(&mut buf, b"mvhd", {
                buf.put_u32(1 << 24);           // version
                buf.put_u64(0);                 // creation_time
                buf.put_u64(0);                 // modification_time
                buf.put_u32(90000);             // timescale
                buf.put_u64(self.tot_duration);
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
            write_box!(&mut buf, b"trak", {
                write_box!(&mut buf, b"tkhd", {
                    buf.put_u32((1 << 24) | 7); // version, flags
                    buf.put_u64(0);             // creation_time
                    buf.put_u64(0);             // modification_time
                    buf.put_u32(1);             // track_id
                    buf.put_u32(0);             // reserved
                    buf.put_u64(self.tot_duration);
                    buf.put_u64(0);             // reserved
                    buf.put_u16(0);             // layer
                    buf.put_u16(0);             // alternate_group
                    buf.put_u16(0);             // volume
                    buf.put_u16(0);             // reserved
                    for v in &[0x00010000,0,0,0,0x00010000,0,0,0,0x40000000] {
                        buf.put_u32(*v);        // matrix
                    }
                    let dims = self.metadata.pixel_dimensions();
                    let width = u32::from(u16::try_from(dims.0)?) << 16;
                    let height = u32::from(u16::try_from(dims.1)?) << 16;
                    buf.put_u32(width);
                    buf.put_u32(height);
                });
                write_box!(&mut buf, b"mdia", {
                    write_box!(&mut buf, b"mdhd", {
                        buf.put_u32(1 << 24);       // version
                        buf.put_u64(0);             // creation_time
                        buf.put_u64(0);             // modification_time
                        buf.put_u32(90000);         // timebase
                        buf.put_u64(self.tot_duration);
                        buf.put_u32(0x55c40000);    // language=und + pre-defined
                    });
                    write_box!(&mut buf, b"hdlr", {
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
                    write_box!(&mut buf, b"minf", {
                        write_box!(&mut buf, b"vmhd", {
                            buf.put_u32(1);
                            buf.put_u64(0);
                        });
                        write_box!(&mut buf, b"dinf", {
                            write_box!(&mut buf, b"dref", {
                                buf.put_u32(0);
                                buf.put_u32(1); // entry_count
                                write_box!(&mut buf, b"url ", {
                                    buf.put_u32(1); // version, flags=self-contained
                                });
                            });
                        });
                        write_box!(&mut buf, b"stbl", {
                            write_box!(&mut buf, b"stsd", {
                                buf.put_u32(0); // version
                                buf.put_u32(1); // entry_count
                                self.write_video_sample_entry(&mut buf, &self.metadata)?;
                            });
                            let samples = u32::try_from(self.durations.len())?;
                            write_box!(&mut buf, b"stts", {
                                buf.put_u32(0);
                                buf.put_u32(samples);
                                for d in &self.durations {
                                    buf.put_u32(1);
                                    buf.put_u32(*d);
                                }
                            });
                            write_box!(&mut buf, b"stsc", {
                                buf.put_u32(0); // version
                                buf.put_u32(1); // entry_count
                                buf.put_u32(1); // first_chunk
                                buf.put_u32(samples);
                                buf.put_u32(1); // sample_description_index
                            });
                            write_box!(&mut buf, b"stsz", {
                                buf.put_u32(0); // version
                                buf.put_u32(0); // sample_size
                                buf.put_u32(samples);
                                for s in &self.sizes {
                                    buf.put_u32(*s);
                                }
                            });
                            write_box!(&mut buf, b"stco", {
                                buf.put_u32(0); // version
                                buf.put_u32(1); // entry_count
                                buf.put_u32(self.mdat_start);
                            });
                            write_box!(&mut buf, b"stss", {
                                buf.put_u32(0); // version
                                buf.put_u32(u32::try_from(self.sync_sample_nums.len())?);
                                for n in &self.sync_sample_nums {
                                    buf.put_u32(*n);
                                }
                            });
                        });
                    });
                });
            });
        });
        self.inner.write_buf(&mut buf.freeze()).await?;
        self.inner.seek(SeekFrom::Start(u64::from(self.mdat_start - 8))).await?;
        self.inner.write_all(&u32::try_from(self.mdat_len)?.to_be_bytes()[..]).await?;
        Ok(())
    }

    fn write_video_sample_entry(&self, buf: &mut BytesMut, metadata: &h264::Metadata) -> Result<(), Error> {
        write_box!(buf, b"avc1", {
            buf.put_u32(0);
            buf.put_u32(1); // data_reference_index = 1
            buf.extend_from_slice(&[0; 16]);
            buf.put_u16(u16::try_from(metadata.pixel_dimensions().0)?);
            buf.put_u16(u16::try_from(metadata.pixel_dimensions().1)?);
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
                buf.extend_from_slice(metadata.avc_decoder_config());
            });
        });
        Ok(())
    }
}

#[async_trait]
impl<W: AsyncWrite + AsyncSeek + Send + Unpin> VideoHandler for Mp4Writer<W> {
    type Metadata = moonfire_rtsp::client::video::h264::Metadata;

    async fn metadata_change(&mut self, metadata: &Self::Metadata) -> Result<(), failure::Error> {
        bail!("metadata change unimplemented. new metadata: {:#?}", metadata)
    }

    async fn picture(&mut self, mut picture: moonfire_rtsp::client::video::Picture) -> Result<(), failure::Error> {
        if let Some(last_pts) = self.last_pts.replace(picture.rtp_timestamp.timestamp) {
            let duration = picture.rtp_timestamp.timestamp.checked_sub(last_pts).unwrap();
            assert!(duration > 0);
            self.durations.push(u32::try_from(duration)?);
            self.tot_duration += duration;
        }
        info!("{}-byte picture", picture.remaining());
        self.sizes.push(u32::try_from(picture.remaining())?);
        if picture.is_random_access_point {
            self.sync_sample_nums.push(u32::try_from(self.sizes.len())?);
        }

        self.mdat_len = u32::try_from(usize::try_from(self.mdat_len)? + picture.remaining())?;

        // TODO: this doesn't use vectored IO. annoying.
        while picture.has_remaining() {
          self.inner.write_buf(&mut picture).await?;
        }
        Ok(())
    }
}

pub async fn run(url: Url, credentials: Option<moonfire_rtsp::client::Credentials>, out: PathBuf) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c();

    // DESCRIBE. https://tools.ietf.org/html/rfc2326#section-10.2
    let mut session = moonfire_rtsp::client::Session::describe(url, credentials).await?;
    let video_stream_i = session.streams().iter()
        .position(|s| s.media == "video")
        .ok_or_else(|| format_err!("couldn't find video stream"))?;
    let video_metadata = session.streams()[video_stream_i].metadata.as_ref().unwrap().clone();
    info!("video metadata: {:#?}", &video_metadata);
    session.setup(video_stream_i).await?;
    let mut session = session.play().await?;

    // Read RTP data.
    let out = tokio::fs::File::create(out).await?;
    let mp4_vid = Mp4Writer::new(video_metadata.clone(), out).await?;
    let to_vid = moonfire_rtsp::client::video::h264::VideoAccessUnitHandler::new(video_metadata.clone(), mp4_vid);
    //let mut print_au = moonfire_rtsp::client::video::h264::PrintAccessUnitHandler::new(&video_metadata)?;
    let video_stream = &session.streams()[video_stream_i];
    let mut h264_timeline = moonfire_rtsp::client::Timeline::new(video_stream.initial_rtptime.unwrap(), video_stream.clock_rate);
    let h264 = moonfire_rtsp::client::video::h264::Handler::new(to_vid);
    let mut h264_rtp = moonfire_rtsp::client::rtp::StrictSequenceChecker::new(
        video_stream.ssrc.unwrap(),
        video_stream.initial_seq.unwrap(),
        h264
    );
    let mut h264_rtcp = moonfire_rtsp::client::rtcp::TimestampPrinter::new();

    let timeout = tokio::time::sleep(super::KEEPALIVE_DURATION);
    tokio::pin!(stop);
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            msg = session.next() => {
                let msg = msg.ok_or_else(|| format_err!("EOF"))??;
                trace!("msg: {:#?}", &msg);
                match msg.msg {
                    rtsp_types::Message::Data(data) => {
                        let c = data.channel_id();
                        let mapping = match session.channel(c) {
                            Some(m) => m,
                            None => bail!("Data message on unexpected channel {} at {:#?}",
                                          c, &msg.ctx),
                        };
                        match mapping.channel_type {
                            ChannelType::Rtp => h264_rtp.data(msg.ctx, &mut h264_timeline, data.into_body()).await?,
                            ChannelType::Rtcp => h264_rtcp.data(msg.ctx, &mut h264_timeline, data.into_body()).await?,
                        };
                    },
                    o => println!("message {:#?}", &o),
                }
            },
            () = &mut timeout => {
                session.send_keepalive().await?;
                timeout.as_mut().reset(tokio::time::Instant::now() + super::KEEPALIVE_DURATION);
            },
            _ = &mut stop => {
                break;
            },
        }
    }
    info!("Stopping");
    let mp4 = h264_rtp.into_inner().into_inner().into_inner();
    mp4.finish().await?;
    Ok(())
}
