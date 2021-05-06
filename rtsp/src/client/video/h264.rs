//! [H.264](https://www.itu.int/rec/T-REC-H.264-201906-I/en)-encoded video.

use std::{cell::RefCell, convert::TryFrom};

use async_trait::async_trait;
use bytes::{Bytes, BytesMut, Buf, BufMut};
use failure::{Error, bail, format_err};
use h264_reader::{annexb::NalReader, nal::{UnitType, sei::SeiIncrementalPayloadReader, slice::SliceLayerWithoutPartitioningRbsp}};
use log::{debug, info, log_enabled, trace};

use crate::client::video::Picture;

use super::VideoHandler;

/// A [super::rtp::PacketHandler] implementation which finds access unit boundaries
/// and produces unfragmented NAL units as specified in [RFC
/// 6184](https://tools.ietf.org/html/rfc6184).
///
/// This doesn't inspect the contents of the NAL units, so it doesn't depend on or
/// verify compliance with H.264 section 7.4.1.2.3 "Order of NAL units and coded
/// pictures and association to access units".
/// 
/// Currently expects that the stream starts at an access unit boundary and has no lost packets.
pub struct Handler<A: AccessUnitHandler> {
    inner: A,

    state: HandlerState,

    /// The largest fragment used. This is used for the buffer capacity on subsequent fragments, minimizing reallocation.
    frag_high_water: usize,
}

struct PreMark {
    timestamp: crate::Timestamp,

    /// If a FU-A fragment is in progress, the buffer used to accumulate the NAL.
    frag_buf: Option<BytesMut>,
}

enum HandlerState {
    /// Not currently processing an access unit.
    Inactive,

    /// Currently processing an access unit.
    /// This will be flushed after a marked packet or when receiving a later timestamp.
    PreMark(PreMark),

    /// Finished processing the given packet. It's an error to receive the same timestamp again.
    PostMark { timestamp: crate::Timestamp },
}

impl<A: AccessUnitHandler> Handler<A> {
    pub fn new(inner: A) -> Self {
        Handler {
            inner,
            state: HandlerState::Inactive,
            frag_high_water: 0,
        }
    }

    pub fn into_inner(self) -> A {
        self.inner
    }
}

#[async_trait]
impl<A: AccessUnitHandler + Send> crate::client::rtp::PacketHandler for Handler<A> {
    async fn end(&mut self) -> Result<(), Error> {
        if let HandlerState::PostMark{..} = self.state {
            self.inner.end().await?;
        }
        Ok(())
    }

    async fn pkt(&mut self, pkt: crate::client::rtp::Packet) -> Result<(), Error> {
        // The rtp crate also has [H.264 depacketization
        // logic](https://docs.rs/rtp/0.2.2/rtp/codecs/h264/struct.H264Packet.html),
        // but it doesn't seem to match my use case. I want to iterate the NALs,
        // not re-encode them in Annex B format.
        let seq = pkt.pkt.header.sequence_number;
        let mut premark = match std::mem::replace(&mut self.state, HandlerState::Inactive) {
            HandlerState::Inactive => {
                self.inner.start(&pkt.rtsp_ctx, pkt.timestamp, &pkt.pkt.header).await?;
                PreMark {
                    timestamp: pkt.timestamp,
                    frag_buf: None
                }
            },
            HandlerState::PreMark(state) => {
                if state.timestamp.timestamp != pkt.timestamp.timestamp {
                    if state.frag_buf.is_some() {
                        bail!("Timestamp changed from {} to {} in the middle of a fragmented NAL at seq={:04x} {:#?}", state.timestamp, pkt.timestamp, seq, &pkt.rtsp_ctx);
                    }
                    self.inner.end().await?;
                }
                state
            },
            HandlerState::PostMark { timestamp: state_ts } => {
                if state_ts.timestamp == pkt.timestamp.timestamp {
                    bail!("Received packet with timestamp {} after marked packet with same timestamp at seq={:04x} {:#?}", pkt.timestamp, seq, &pkt.rtsp_ctx);
                }
                self.inner.end().await?;
                self.inner.start(&pkt.rtsp_ctx, pkt.timestamp, &pkt.pkt.header).await?;
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
                self.inner.nal(data).await?;
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
                if (start && end) || reserved {
                    bail!("Invalid FU-A header {:08b} at seq {:04x} {:#?}", fu_header, seq, &pkt.rtsp_ctx);
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
                            self.inner.nal(frag_buf.freeze()).await?;
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
            self.state = HandlerState::PostMark { timestamp: pkt.timestamp };
        } else {
            self.state = HandlerState::PreMark(premark);
        }
        Ok(())
    }
}

/// Processes H.264 access units and NALs.
#[async_trait]
pub trait AccessUnitHandler {
    /// Starts an access unit.
    async fn start(&mut self, rtsp_ctx: &crate::Context, timestamp: crate::Timestamp, hdr: &rtp::header::Header) -> Result<(), Error>;

    /// Processes a single NAL.
    /// Must be between `start` and `end` calls. `nal` is guaranteed to have a header byte.
    async fn nal(&mut self, nal: Bytes) -> Result<(), Error>;

    /// Ends an access unit.
    async fn end(&mut self) -> Result<(), Error>;
}

/// Produces [VideoHandler] events from [AccessUnitHandler] events.
/// Currently this is a naïve implementation which assumes each access unit has a single slice.
pub struct VideoAccessUnitHandler<V: VideoHandler<Metadata = Metadata>> {
    metadata: Metadata,
    state: VideoState,
    inner: V,
}

enum VideoState {
    Unstarted,
    Started {
        timestamp: crate::Timestamp,
        new_sps: Option<Bytes>,
        new_pps: Option<Bytes>,
    },
    SentPicture,
}

impl<V: VideoHandler<Metadata = Metadata> + Send> VideoAccessUnitHandler<V> {
    pub fn new(metadata: Metadata, inner: V) -> Self {
        Self {
            metadata,
            state: VideoState::Unstarted,
            inner,
        }
    }

    pub fn into_inner(self) -> V {
        self.inner
    }
}

#[async_trait]
impl<V: VideoHandler<Metadata = Metadata> + Send> AccessUnitHandler for VideoAccessUnitHandler<V> {
    async fn start(&mut self, _rtsp_ctx: &crate::Context, timestamp: crate::Timestamp, _hdr: &rtp::header::Header) -> Result<(), Error> {
        if !matches!(self.state, VideoState::Unstarted) {
            bail!("access unit started in invalid state");
        }
        self.state = VideoState::Started {
            timestamp,
            new_sps: None,
            new_pps: None,
        };
        Ok(())
    }

    async fn nal(&mut self, nal: Bytes) -> Result<(), Error> {
        let nal_header = h264_reader::nal::NalHeader::new(nal[0]).map_err(|e| format_err!("bad NAL header 0x{:x}: {:#?}", nal[0], e))?;
        let (timestamp, new_sps, new_pps) = match &mut self.state {
            VideoState::Unstarted => bail!("NAL outside access unit"),
            VideoState::SentPicture => bail!("currently expects a single slice to end the access unit, got {:?}", nal_header),
            VideoState::Started { timestamp, new_sps, new_pps } => (timestamp, new_sps, new_pps),
        };
        let unit_type = nal_header.nal_unit_type();
        match unit_type {
            UnitType::SeqParameterSet => {
                if new_sps.is_some() {
                    bail!("multiple SPSs in access unit");
                }
                if nal != self.metadata.sps_nal() {
                    *new_sps = Some(nal);
                }
            },
            UnitType::PicParameterSet => {
                if new_pps.is_some() {
                    bail!("multiple PPSs in access unit");
                }
                if nal != self.metadata.pps_nal() {
                    *new_pps = Some(nal);
                }
            },
            UnitType::SliceLayerWithoutPartitioningIdr | UnitType::SliceLayerWithoutPartitioningNonIdr => {
                if new_sps.is_some() || new_pps.is_some() {
                    let sps_nal = new_sps.as_ref().map(|b| &b[..]).unwrap_or(self.metadata.sps_nal());
                    let pps_nal = new_pps.as_ref().map(|b| &b[..]).unwrap_or(self.metadata.pps_nal());
                    let new_metadata = Metadata::from_sps_and_pps(sps_nal, pps_nal)?;
                    self.inner.metadata_change(&new_metadata).await?;
                    self.metadata = new_metadata;
                }
                let rtp_timestamp = *timestamp;
                self.state = VideoState::SentPicture;
                self.inner.picture(Picture {
                    rtp_timestamp,
                    is_random_access_point: unit_type == UnitType::SliceLayerWithoutPartitioningIdr,
                    is_disposable: nal_header.nal_ref_idc() == 0,
                    pos: 0,
                    data_prefix: u32::try_from(nal.len()).unwrap().to_be_bytes(),
                    data: nal,
                }).await?;
            },
            _ => {},
        }
        Ok(())
    }

    async fn end(&mut self) -> Result<(), Error> {
        if !matches!(self.state, VideoState::SentPicture) {
            bail!("access unit ended in invalid state");
        }
        self.state = VideoState::Unstarted;
        Ok(())
    }
}

pub struct PrintAccessUnitHandler {
    ctx: h264_reader::Context<()>,
}

struct HeaderPrinter;

impl SeiIncrementalPayloadReader for HeaderPrinter {
    type Ctx = ();

    fn start(&mut self, _ctx: &mut h264_reader::Context<Self::Ctx>, payload_type: h264_reader::nal::sei::HeaderType, payload_size: u32) {
        trace!("  SEI payload type={:?} size={}", &payload_type, payload_size);
    }

    fn push(&mut self, _ctx: &mut h264_reader::Context<Self::Ctx>, buf: &[u8]) {
        use pretty_hex::PrettyHex;
        trace!("SEI: {:?}", buf.hex_dump());
    }
    fn end(&mut self, _ctx: &mut h264_reader::Context<Self::Ctx>) {}
    fn reset(&mut self, _ctx: &mut h264_reader::Context<Self::Ctx>) {}
}

impl PrintAccessUnitHandler {
    pub fn new(metadata: &Metadata) -> Result<Self, Error> {
        let config = h264_reader::avcc::AvcDecoderConfigurationRecord::try_from(&metadata.avc_decoder_config[..])
            .map_err(|e| format_err!("{:?}", e))?;
        let ctx = config.create_context(())
            .map_err(|e| format_err!("{:?}", e))?;
        Ok(PrintAccessUnitHandler {
            ctx,
        })
    }
}

#[async_trait]
impl AccessUnitHandler for PrintAccessUnitHandler {
    async fn start(&mut self, _rtsp_ctx: &crate::Context, timestamp: crate::Timestamp, _hdr: &rtp::header::Header) -> Result<(), Error> {
        info!("access unit with timestamp {}:", timestamp);
        Ok(())
    }

    async fn nal(&mut self, nal: Bytes) -> Result<(), Error> {
        let nal_header = h264_reader::nal::NalHeader::new(nal[0]).map_err(|e| format_err!("bad NAL header 0x{:x}: {:#?}", nal[0], e))?;
        info!("  nal ref_idc={} type={} ({:?}) size={}", nal_header.nal_ref_idc(), nal_header.nal_unit_type().id(), nal_header.nal_unit_type(), nal.len());
        if log_enabled!(log::Level::Trace) {
            let sei_handler = h264_reader::nal::sei::SeiNalHandler::new(HeaderPrinter);
            let mut nal_switch = h264_reader::nal::NalSwitch::default();
            nal_switch.put_handler(UnitType::SEI, Box::new(RefCell::new(sei_handler)));
            nal_switch.put_handler(UnitType::SliceLayerWithoutPartitioningIdr, Box::new(RefCell::new(SliceLayerWithoutPartitioningRbsp::default())));
            nal_switch.put_handler(UnitType::SliceLayerWithoutPartitioningNonIdr, Box::new(RefCell::new(SliceLayerWithoutPartitioningRbsp::default())));
            nal_switch.start(&mut self.ctx);
            nal_switch.push(&mut self.ctx, &nal[..]);
            nal_switch.end(&mut self.ctx);
        }
        Ok(())
    }

    async fn end(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

/// Decodes a NAL unit (minus header byte) into its RBSP.
/// Stolen from h264-reader's src/avcc.rs. This shouldn't last long, see:
/// <https://github.com/dholroyd/h264-reader/issues/4>.
fn decode(encoded: &[u8]) -> Vec<u8> {
    struct NalRead(Vec<u8>);
    use h264_reader::nal::NalHandler;
    use h264_reader::Context;
    impl NalHandler for NalRead {
        type Ctx = ();
        fn start(&mut self, _ctx: &mut Context<Self::Ctx>, _header: h264_reader::nal::NalHeader) {}

        fn push(&mut self, _ctx: &mut Context<Self::Ctx>, buf: &[u8]) {
            self.0.extend_from_slice(buf)
        }

        fn end(&mut self, _ctx: &mut Context<Self::Ctx>) {}
    }
    let mut decode = h264_reader::rbsp::RbspDecoder::new(NalRead(vec![]));
    let mut ctx = Context::new(());
    decode.push(&mut ctx, encoded);
    let read = decode.into_handler();
    read.0
}

#[derive(Clone)]
pub struct Metadata {
    pixel_dimensions: (u32, u32),
    rfc6381_codec: String,
    pixel_aspect_ratio: Option<(u32, u32)>,
    frame_rate: Option<(u32, u32)>,
    avc_decoder_config: Vec<u8>,

    /// The SPS NAL, as a range within [avc_decoder_config].
    sps_nal: std::ops::Range<usize>,

    /// The PPS NAL, as a range within [avc_decoder_config].
    pps_nal: std::ops::Range<usize>,
}

impl std::fmt::Debug for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use pretty_hex::PrettyHex;
        f.debug_struct("Metadata")
         .field("rfc6381_codec", &self.rfc6381_codec)
         .field("pixel_dimensions", &self.pixel_dimensions)
         .field("pixel_aspect_ratio", &self.pixel_aspect_ratio)
         .field("frame_rate", &self.frame_rate)
         .field("avc_decoder_config", &self.avc_decoder_config.hex_dump())
         .finish()
    }
}

impl Metadata {
    /// Parses metadata from the `format-specific-params` of a SDP `fmtp` media attribute.
    pub fn from_format_specific_params(format_specific_params: &str) -> Result<Self, Error> {
        let mut sprop_parameter_sets = None;
        for p in format_specific_params.split(';') {
            let (key, value) = crate::client::parse::split_once(p.trim(), '=').unwrap();
            if key == "sprop-parameter-sets" {
                sprop_parameter_sets = Some(value);
            }
        }
        let sprop_parameter_sets = sprop_parameter_sets
            .ok_or_else(|| format_err!("no sprop-parameter-sets in H.264 format-specific-params"))?;

        let mut sps_nal = None;
        let mut pps_nal = None;
        for nal in sprop_parameter_sets.split(',') {
            let nal = base64::decode(nal).map_err(|_| format_err!("NAL has invalid base64 encoding"))?;
            if nal.is_empty() {
                bail!("empty NAL");
            }
            let header = h264_reader::nal::NalHeader::new(nal[0]).map_err(|_| format_err!("bad NAL header {:0x}", nal[0]))?;
            match header.nal_unit_type() {
                UnitType::SeqParameterSet => {
                    if sps_nal.is_some() {
                        bail!("multiple SPSs");
                    }
                    sps_nal = Some(nal);
                },
                UnitType::PicParameterSet => {
                    if pps_nal.is_some() {
                        bail!("multiple PPSs");
                    }
                    pps_nal = Some(nal);
                },
                _ => bail!("only SPS and PPS expected in parameter sets"),
            }
        }
        let sps_nal = sps_nal.ok_or_else(|| format_err!("no sps"))?;
        let pps_nal = pps_nal.ok_or_else(|| format_err!("no pps"))?;
        Self::from_sps_and_pps(&sps_nal[..], &pps_nal[..])
    }

    fn from_sps_and_pps(sps_nal: &[u8], pps_nal: &[u8]) -> Result<Self, Error> {
        let sps_rbsp = decode(&sps_nal[1..]);
        if sps_rbsp.len() < 4 {
            bail!("bad sps");
        }
        let rfc6381_codec = format!("avc1.{:02X}{:02X}{:02X}", sps_rbsp[0], sps_rbsp[1], sps_rbsp[2]);
        let sps = h264_reader::nal::sps::SeqParameterSet::from_bytes(&sps_rbsp)
            .map_err(|e| format_err!("Bad SPS: {:?}", e))?;
        debug!("sps: {:#?}", &sps);

        let pixel_dimensions = sps.pixel_dimensions().map_err(|e| format_err!("SPS has invalid pixel dimensions: {:?}", e))?;

        // Create the AVCDecoderConfiguration, ISO/IEC 14496-15 section 5.2.4.1.
        // The beginning of the AVCDecoderConfiguration takes a few values from
        // the SPS (ISO/IEC 14496-10 section 7.3.2.1.1).
        let mut avc_decoder_config = Vec::with_capacity(11 + sps_nal.len() + pps_nal.len());
        avc_decoder_config.push(1); // configurationVersion
        avc_decoder_config.extend(&sps_rbsp[0..=2]); // profile_idc . AVCProfileIndication
                                                     // ...misc bits... . profile_compatibility
                                                     // level_idc . AVCLevelIndication

        // Hardcode lengthSizeMinusOne to 3, matching TransformSampleData's 4-byte
        // lengths.
        avc_decoder_config.push(0xff);

        // Only support one SPS and PPS.
        // ffmpeg's ff_isom_write_avcc has the same limitation, so it's probably
        // fine. This next byte is a reserved 0b111 + a 5-bit # of SPSs (1).
        avc_decoder_config.push(0xe1);
        avc_decoder_config.extend(&u16::try_from(sps_nal.len())?.to_be_bytes()[..]);
        let sps_nal_start = avc_decoder_config.len();
        avc_decoder_config.extend_from_slice(&sps_nal[..]);
        let sps_nal_end = avc_decoder_config.len();
        avc_decoder_config.push(1); // # of PPSs.
        avc_decoder_config.extend(&u16::try_from(pps_nal.len())?.to_be_bytes()[..]);
        let pps_nal_start = avc_decoder_config.len();
        avc_decoder_config.extend_from_slice(&pps_nal[..]);
        let pps_nal_end = avc_decoder_config.len();
        assert_eq!(avc_decoder_config.len(), 11 + sps_nal.len() + pps_nal.len());

        let (pixel_aspect_ratio, frame_rate);
        match sps.vui_parameters {
            Some(ref vui) => {
                pixel_aspect_ratio = vui.aspect_ratio_info.as_ref().and_then(|a| a.clone().get()).map(|(h, v)| (u32::from(h), (u32::from(v))));

                // TODO: study H.264, (E-34). This quick'n'dirty calculation isn't always right.
                frame_rate = vui.timing_info.as_ref().map(|t| (2 * t.num_units_in_tick, t.time_scale));
            },
            None => {
                pixel_aspect_ratio = None;
                frame_rate = None;
            },
        }
        Ok(Metadata {
            avc_decoder_config,
            pixel_dimensions,
            rfc6381_codec,
            pixel_aspect_ratio,
            frame_rate,
            sps_nal: sps_nal_start..sps_nal_end,
            pps_nal: pps_nal_start..pps_nal_end,
        })
    }

    fn sps_nal(&self) -> &[u8] {
        &self.avc_decoder_config[self.sps_nal.clone()]
    }

    fn pps_nal(&self) -> &[u8] {
        &self.avc_decoder_config[self.pps_nal.clone()]
    }

    pub fn avc_decoder_config(&self) -> &[u8] {
        &self.avc_decoder_config
    }
}

impl super::Metadata for Metadata {
    fn pixel_dimensions(&self) -> (u32, u32) {
        self.pixel_dimensions
    }

    fn pixel_aspect_ratio(&self) -> Option<(u32, u32)> {
        self.pixel_aspect_ratio
    }

    fn rfc6381_codec(&self) -> &str {
        &self.rfc6381_codec
    }

    fn frame_rate(&self) -> Option<(u32, u32)> {
        self.frame_rate
    }
}
