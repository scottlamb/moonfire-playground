use std::{borrow::Cow, fmt::Debug, num::{NonZeroU16, NonZeroU8}, pin::Pin};

use async_stream::try_stream;
use bytes::Bytes;
use failure::{Error, bail, format_err};
use futures::{SinkExt, StreamExt};
use log::{debug, trace, warn};
use pin_project::pin_project;
use sdp::session_description::SessionDescription;
use tokio::pin;
use tokio_util::codec::Framed;
use url::Url;

use crate::{Context, Timestamp, codec::CodecItem};

mod parse;
pub mod rtcp;
pub mod rtp;

const MAX_FORWARD_TIME_JUMP_SECS: u32 = 10;

/// Duration between keepalive RTSP requests during [Playing] state.
pub const KEEPALIVE_DURATION: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Debug)]
pub struct Presentation {
    pub streams: Vec<Stream>,
    base_url: Url,
    pub control: Url,
    pub accept_dynamic_rate: bool,
    sdp: SessionDescription,
}

/// Information about a stream offered within a presentation.
/// Currently if multiple formats are offered, this only describes the first.
#[derive(Debug)]
pub struct Stream {
    /// Media type, as specified in the [IANA SDP parameters media
    /// registry](https://www.iana.org/assignments/sdp-parameters/sdp-parameters.xhtml#sdp-parameters-1).
    pub media: String,

    /// An encoding name, as specified in the [IANA media type
    /// registry](https://www.iana.org/assignments/media-types/media-types.xhtml), with
    /// ASCII characters in lowercase.
    ///
    /// Commonly used but not specified in that registry: the ONVIF types
    /// claimed in the
    /// [ONVIF Streaming Spec](https://www.onvif.org/specs/stream/ONVIF-Streaming-Spec.pdf):
    /// *   `vnd.onvif.metadata`
    /// *   `vnd.onvif.metadata.gzip`,
    /// *   `vnd.onvif.metadata.exi.onvif`
    /// *   `vnd.onvif.metadata.exi.ext`
    pub encoding_name: String,

    /// RTP payload type.
    /// See the [registry](https://www.iana.org/assignments/rtp-parameters/rtp-parameters.xhtml#rtp-parameters-1).
    /// It's common to use one of the dynamically assigned values, 96–127.
    pub rtp_payload_type: u8,

    /// RTP clock rate, in Hz.
    pub clock_rate: u32,

    /// Number of audio channels, if applicable (`media` is `audio`) and known.
    pub channels: Option<NonZeroU16>,

    demuxer: Result<crate::codec::Demuxer, Error>,

    /// The specified control URL.
    /// This is needed with multiple streams to send `SETUP` requests and
    /// interpret the `PLAY` response's `RTP-Info` header.
    /// [RFC 2326 section C.3](https://datatracker.ietf.org/doc/html/rfc2326#appendix-C.3)
    /// says the server is allowed to omit it when there is only a single stream.
    pub control: Option<Url>,

    /// Some buggy cameras expect the base URL to be interpreted as if it had an
    /// implicit trailing slash. (This is approximately what ffmpeg 4.3.1 does
    /// when the base URL has a query string.) If `RTP-Info` matching fails, try
    /// again with this URL.
    alt_control: Option<Url>,

    state: StreamState,
}

impl Stream {
    /// Returns the parameters for this stream.
    ///
    /// Returns `None` on unknown codecs, bad parameters, or if parameters aren't specified
    /// via SDP. Some codecs allow parameters to be specified in-band instead.
    pub fn parameters(&self) -> Option<&crate::codec::Parameters> {
        self.demuxer.as_ref().ok().and_then(|d| d.parameters())
    }
}

#[derive(Debug)]
enum StreamState {
    /// Uninitialized; no `SETUP` has yet been sent.
    Uninit,

    /// `SETUP` reply has been received.
    Init(StreamStateInit),

    /// `PLAY` reply has been received.
    Playing {
        timeline: Timeline,
        rtp_handler: rtp::StrictSequenceChecker,
        rtcp_handler: rtcp::TimestampPrinter,
    }
}

#[derive(Copy, Clone, Debug, Default)]
struct StreamStateInit {
    /// The RTP synchronization source (SSRC), as defined in
    /// [RFC 3550](https://tools.ietf.org/html/rfc3550). This is normally
    /// supplied in the `SETUP` response's `Transport` header. Reolink cameras
    /// instead supply it in the `PLAY` response's `RTP-Info` header.
    ssrc: Option<u32>,

    /// The initial RTP sequence number, as specified in the `PLAY` response's
    /// `RTP-Info` header. This field is only used during the `play()` call
    /// itself; by the time it returns, the stream will be in state `Playing`.
    initial_seq: Option<u16>,

    /// The initial RTP timestamp, as specified in the `PLAY` response's
    /// `RTP-Info` header. This field is only used during the `play()` call
    /// itself; by the time it returns, the stream will be in state `Playing`.
    initial_rtptime: Option<u32>,
}

/// Creates [Timestamp]s (which don't wrap and can be converted to NPT aka normal play time)
/// from 32-bit (wrapping) RTP timestamps.
#[derive(Debug)]
struct Timeline {
    timestamp: u64,
    clock_rate: u32,
    start: Option<u32>,
    max_forward_jump: u32,
}

impl Timeline {
    /// Creates a new timeline, erroring on crazy clock rates.
    fn new(start: Option<u32>, clock_rate: u32) -> Result<Self, Error> {
        if clock_rate == 0 {
            bail!("clock_rate=0 rejected to prevent division by zero");
        }
        let max_forward_jump = MAX_FORWARD_TIME_JUMP_SECS
            .checked_mul(clock_rate)
            .ok_or_else(|| format_err!(
                "clock_rate={} rejected because max forward jump of {} sec exceeds u32::MAX",
                clock_rate, MAX_FORWARD_TIME_JUMP_SECS))?;
        Ok(Timeline {
            timestamp: u64::from(start.unwrap_or(0)),
            start,
            clock_rate,
            max_forward_jump,
        })
    }

    /// Advances to the given (wrapping) RTP timestamp, creating a monotonically
    /// increasing [Timestamp]. Errors on excessive or backward time jumps.
    fn advance_to(&mut self, rtp_timestamp: u32) -> Result<Timestamp, Error> {
        let start = match self.start {
            None => {
                self.start = Some(rtp_timestamp);
                self.timestamp = u64::from(rtp_timestamp);
                rtp_timestamp
            },
            Some(start) => start,
        };
        let forward_delta = rtp_timestamp.wrapping_sub(self.timestamp as u32);
        let forward_ts = Timestamp {
            timestamp: self.timestamp.checked_add(u64::from(forward_delta)).ok_or_else(|| {
                // This probably won't happen even with a hostile server. It'd
                // take (2^32 - 1) packets (~ 4 billion) to advance the time
                // this far, even with a clock rate chosen to maximize
                // max_forward_jump for our MAX_FORWARD_TIME_JUMP_SECS.
                format_err!("timestamp {} + {} will exceed u64::MAX!",
                            self.timestamp, forward_delta)
            })?,
            clock_rate: self.clock_rate,
            start,
        };
        if forward_delta > self.max_forward_jump {
            let f64_clock_rate = f64::from(self.clock_rate);
            let backward_delta = (self.timestamp as u32).wrapping_sub(rtp_timestamp);
            bail!("Timestamp jumped:\n\
                  * forward by  {:10} ({:10.03} sec) from {} to {}, more than allowed {} sec OR\n\
                  * backward by {:10} ({:10.03} sec), more than allowed 0 sec",
                  forward_delta, (forward_delta as f64) / f64_clock_rate, self.timestamp,
                  forward_ts, MAX_FORWARD_TIME_JUMP_SECS, backward_delta,
                  (backward_delta as f64) / f64_clock_rate);
        }
        self.timestamp = forward_ts.timestamp;
        Ok(forward_ts)
    }
}


pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum ChannelType {
    Rtp,
    Rtcp,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct ChannelMapping {
    stream_i: usize,
    channel_type: ChannelType,
}

/// Mapping of the 256 possible RTSP interleaved channels to stream indices and
/// RTP/RTCP. Assumptions:
/// *   We only need to support 255 possible streams in a presentation. If
///     there are more than 128, we couldn't actually stream them all at once
///     anyway with one RTP and one RTCP channel per stream.
/// *   We'll always assign even channels numbers as RTP and their odd
///     successors as RTCP for the same stream. This seems reasonable given
///     that there is no clear way to assign a single channel in the RTSP spec.
///     [RFC 2326 section 10.12](https://tools.ietf.org/html/rfc2326#section-10.12)
///     says that `interleaved=n` also assigns channel `n+1`, and it's ambiguous
///     what `interleaved=n-m` does when `m > n+1` (section 10.12 suggests it
///     assigns only `n` and `m`; section 12.39 the suggests full range `[n,
///     m]`) or when `n==m`. We'll get into trouble if an RTSP server insists on
///     specifying an odd `n`, but that just seems obstinate.
/// These assumptions let us keep the full mapping with little space and an
/// efficient lookup operation.
#[derive(Default)]
struct ChannelMappings(smallvec::SmallVec<[Option<NonZeroU8>; 16]>);

impl ChannelMappings {
    /// Returns the next unassigned even channel id, or errors.
    fn next_unassigned(&self) -> Result<u8, Error> {
        if let Some(i) = self.0.iter().position(Option::is_none) {
            return Ok((i as u8) << 1);
        }
        if self.0.len() < 128 {
            return Ok((self.0.len() as u8) << 1);
        }
        bail!("all RTSP channels have been assigned");
    }

    /// Assigns an even channel id (to RTP) and its odd successor (to RTCP) or errors.
    fn assign(&mut self, channel_id: u8, stream_i: usize) -> Result<(), Error> {
        if (channel_id & 1) != 0 {
            bail!("Can't assign odd channel id {}", channel_id);
        }
        if stream_i >= 255 {
            bail!("Can't assign channel to stream id {} because it's >= 255", stream_i);
        }
        let i = usize::from(channel_id >> 1);
        if i >= self.0.len() {
            self.0.resize(i + 1, None);
        }
        let c = &mut self.0[i];
        if let Some(c) = c {
            bail!("Channel id {} is already assigned to stream {}; won't reassign to stream {}",
                  channel_id, c.get() - 1, channel_id);
        }
        *c = Some(NonZeroU8::new((stream_i + 1) as u8).expect("[0, 255) + 1 is non-zero"));
        Ok(())
    }

    /// Looks up a channel id's mapping.
    fn lookup(&self, channel_id: u8) -> Option<ChannelMapping> {
        let i = usize::from(channel_id >> 1);
        if i >= self.0.len() {
            return None;
        }
        self.0[i].map(|c| ChannelMapping {
            stream_i: usize::from(c.get() - 1),
            channel_type: match (channel_id & 1) != 0 {
                false => ChannelType::Rtp,
                true => ChannelType::Rtcp,
            }
        })
    }
}

impl Debug for ChannelMappings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.0.iter().enumerate().filter_map(|(i, v)| v.map(|v| (
                format!("{}-{}", i << 1, (i << 1) + 1),
                v.get() - 1
            ))))
            .finish()
    }
}

/// Marker trait for the state of a [Session].
/// This doesn't closely match [RFC 2326
/// A.1](https://tools.ietf.org/html/rfc2326#appendix-A.1). In practice, we've
/// found that cheap IP cameras are more restrictive than RTSP suggests. Eg, a
/// `DESCRIBE` changes the connection's state such that another one will fail,
/// before assigning a session id. Thus [Session] represents something more like
/// an RTSP connection than an RTSP session.
pub trait State {}

/// Initial state after a `DESCRIBE`.
/// One or more `SETUP`s may have also been issued, in which case a `session_id`
/// will be assigned.
pub struct Described {
    presentation: Presentation,
    session_id: Option<String>,
    channels: ChannelMappings,
}
impl State for Described {}

/// State after a `PLAY`.
#[pin_project(project = PlayingProj)]
pub struct Playing {
    presentation: Presentation,
    session_id: String,
    channels: ChannelMappings,
    pending_keepalive_cseq: Option<u32>,

    #[pin]
    keepalive_timer: tokio::time::Sleep,
}
impl State for Playing {}

/// The raw connection, without tracking session state.
struct RtspConnection {
    creds: Option<Credentials>,
    requested_auth: Option<digest_auth::WwwAuthenticateHeader>,
    stream: Framed<tokio::net::TcpStream, crate::Codec>,
    user_agent: String,

    /// The next `CSeq` header value to use when sending an RTSP request.
    next_cseq: u32,
}

/// An RTSP session, or a connection that may be used in a proscriptive way.
/// See discussion at [State].
#[pin_project]
pub struct Session<S: State> {
    conn: RtspConnection,

    #[pin]
    state: S,
}

impl RtspConnection {
    async fn connect(url: &Url, creds: Option<Credentials>) -> Result<Self, Error> {
        if url.scheme() != "rtsp" {
            bail!("Only rtsp urls supported (no rtsps yet)");
        }
        if url.username() != "" || url.password().is_some() {
            // Url apparently doesn't even have a way to clear the credentials,
            // so this has to be an error.
            bail!("URL must not contain credentials");
        }
        let host = url.host_str().ok_or_else(|| format_err!("Must specify host in rtsp url {}", &url))?;
        let port = url.port().unwrap_or(554);
        let stream = tokio::net::TcpStream::connect((host, port)).await?;
        let conn_established = time::get_time();
        let conn_local_addr = stream.local_addr()?;
        let conn_peer_addr = stream.peer_addr()?;
        let stream = Framed::new(stream, crate::Codec {
            ctx: crate::Context {
                conn_established,
                conn_local_addr,
                conn_peer_addr,
                msg_pos: 0,
                msg_received: conn_established,
            },
        });
        Ok(Self {
            creds,
            requested_auth: None,
            stream,
            user_agent: "moonfire-rtsp test".to_string(),
            next_cseq: 1,
        })
    }

    /// Sends a request and expects the next message from the peer to be its response.
    /// Takes care of authorization and `CSeq`. Returns `Error` if not successful.
    async fn send(&mut self, req: &mut rtsp_types::Request<Bytes>) -> Result<rtsp_types::Response<Bytes>, Error> {
        loop {
            let cseq = self.send_nowait(req).await?;
            let msg = self.stream.next().await.ok_or_else(|| format_err!("unexpected EOF while waiting for reply"))??;
            let resp = match msg.msg {
                rtsp_types::Message::Response(r) => r,
                o => bail!("Unexpected RTSP message {:?}", &o),
            };
            if parse::get_cseq(&resp) != Some(cseq) {
                bail!("didn't get expected CSeq {:?} on {:?} at {:#?}", &cseq, &resp, &msg.ctx);
            }
            if resp.status() == rtsp_types::StatusCode::Unauthorized {
                if self.requested_auth.is_some() {
                    bail!("Received Unauthorized after trying digest auth at {:#?}", &msg.ctx);
                }
                let www_authenticate = match resp.header(&rtsp_types::headers::WWW_AUTHENTICATE) {
                    None => bail!("Unauthorized without WWW-Authenticate header at {:#?}", &msg.ctx),
                    Some(h) => h,
                };
                let www_authenticate = www_authenticate.as_str();
                if !www_authenticate.starts_with("Digest ") {
                    bail!("Non-digest authentication requested at {:#?}", &msg.ctx);
                }
                let www_authenticate = digest_auth::WwwAuthenticateHeader::parse(www_authenticate)?;
                self.requested_auth = Some(www_authenticate);
                continue;
            } else if !resp.status().is_success() {
                bail!("RTSP {:?} request returned {} at {:#?}", req.method(), resp.status(), &msg.ctx);
            }
            return Ok(resp);
        }
    }

    /// Sends a request without waiting for a response, returning the `CSeq`.
    async fn send_nowait(&mut self, req: &mut rtsp_types::Request<Bytes>) -> Result<u32, Error> {
        let cseq = self.next_cseq;
        self.next_cseq += 1;
        match (self.requested_auth.as_mut(), self.creds.as_ref()) {
            (None, _) => {},
            (Some(auth), Some(creds)) => {
                let uri = req.request_uri().map(|u| u.as_str()).unwrap_or("*");
                let method = digest_auth::HttpMethod(Cow::Borrowed(req.method().into()));
                let ctx = digest_auth::AuthContext::new_with_method(
                    &creds.username, &creds.password, uri, Option::<&'static [u8]>::None, method);
                let authorization = auth.respond(&ctx)?.to_string();
                req.insert_header(rtsp_types::headers::AUTHORIZATION, authorization);
            },
            (Some(_), None) => bail!("Authentication required; no credentials supplied"),
        }
        req.insert_header(rtsp_types::headers::CSEQ, cseq.to_string());
        req.insert_header(rtsp_types::headers::USER_AGENT, self.user_agent.clone());
        self.stream.send(rtsp_types::Message::Request(req.clone())).await?;
        Ok(cseq)
    }
}

impl Session<Described> {
    pub async fn describe(url: Url, creds: Option<Credentials>) -> Result<Self, Error> {
        let mut conn = RtspConnection::connect(&url, creds).await?;
        let mut req = rtsp_types::Request::builder(rtsp_types::Method::Describe, rtsp_types::Version::V1_0)
            .header(rtsp_types::headers::ACCEPT, "application/sdp")
            .request_uri(url.clone())
            .build(Bytes::new());
        let response = conn.send(&mut req).await?;
        let presentation = parse::parse_describe(url, response)?;
        Ok(Session {
            conn,
            state: Described {
                presentation,
                session_id: None,
                channels: ChannelMappings::default(),
            },
        })
    }

    pub fn streams(&self) -> &[Stream] { &self.state.presentation.streams }

    /// Sends a `SETUP` request for a stream.
    /// Note these can't reasonably be pipelined because subsequent requests
    /// are expected to adopt the previous response's `Session`. Likewise,
    /// the server may override the preferred interleaved channel id and it
    /// seems like a bad idea to try to assign more interleaved channels without
    /// inspect that first.
    ///
    /// Panics if `stream_i >= self.streams().len()`.
    pub async fn setup(&mut self, stream_i: usize) -> Result<(), Error> {
        let stream = &mut self.state.presentation.streams[stream_i];
        if !matches!(stream.state, StreamState::Uninit) {
            bail!("stream already set up");
        }
        let proposed_channel_id = self.state.channels.next_unassigned()?;
        let url = stream.control.as_ref().unwrap_or(&self.state.presentation.control).clone();
        let mut req = rtsp_types::Request::builder(rtsp_types::Method::Setup, rtsp_types::Version::V1_0)
            .request_uri(url)
            .header(
                rtsp_types::headers::TRANSPORT,
                format!(
                    "RTP/AVP/TCP;unicast;interleaved={}-{}",
                    proposed_channel_id,
                    proposed_channel_id + 1)
            )
            .header(crate::X_DYNAMIC_RATE.clone(), "1".to_owned());
        if let Some(ref s) = self.state.session_id {
            req = req.header(rtsp_types::headers::SESSION, s.clone());
        }
        let response = self.conn.send(&mut req.build(Bytes::new())).await?;
        debug!("SETUP response: {:#?}", &response);
        let response = parse::parse_setup(&response)?;
        match self.state.session_id.as_ref() {
            Some(old) if old != response.session_id => {
                bail!("SETUP response changed session id from {:?} to {:?}",
                      old, response.session_id);
            },
            Some(_) => {},
            None => self.state.session_id = Some(response.session_id.to_owned()),
        };
        self.state.channels.assign(response.channel_id, stream_i)?;
        stream.state = StreamState::Init(StreamStateInit {
            ssrc: response.ssrc,
            initial_seq: None,
            initial_rtptime: None,
        });
        Ok(())
    }

    /// Sends a `PLAY` request for the entire presentation.
    /// The presentation must support aggregate control, as defined in [RFC 2326
    /// section 1.3](https://tools.ietf.org/html/rfc2326#section-1.3).
    pub async fn play(mut self) -> Result<Session<Playing>, Error> {
        let session_id = self.state.session_id.take().ok_or_else(|| format_err!("must SETUP before PLAY"))?;
        trace!("PLAY with channel mappings: {:#?}", &self.state.channels);
        let response = self.conn.send(
            &mut rtsp_types::Request::builder(rtsp_types::Method::Play, rtsp_types::Version::V1_0)
            .request_uri(self.state.presentation.control.clone())
            .header(rtsp_types::headers::SESSION, session_id.clone())
            .header(rtsp_types::headers::RANGE, "npt=0.000-".to_owned())
            .build(Bytes::new())).await?;
        parse::parse_play(response, &mut self.state.presentation)?;

        // Count how many streams have been setup (not how many are in the presentation).
        let setup_streams = self.state.presentation.streams.iter()
            .filter(|s| matches!(s.state, StreamState::Init(_)))
            .count();

        // Move all streams that have been set up from Init to Playing state. Check that required
        // parameters are present while doing so.
        for (i, s) in self.state.presentation.streams.iter_mut().enumerate() {
            match s.state {
                StreamState::Init(StreamStateInit {
                    initial_rtptime,
                    initial_seq,
                    ssrc,
                    ..
                }) => {
                    // The initial rtptime is useful for syncing multiple streams:
                    let initial_rtptime = match setup_streams {
                        // If there's only a single stream, don't require or use
                        // it. Buggy cameras (GW4089IP) specify bogus values.
                        1 => None,
                        _ => {
                            //if initial_rtptime.is_none() {
                            //    bail!("Missing rtptime after PLAY response, stream {}/{:#?}", i, s);
                            //}
                            initial_rtptime
                        },
                    };
                    s.state = StreamState::Playing {
                        timeline: Timeline::new(initial_rtptime, s.clock_rate)?,
                        rtp_handler: rtp::StrictSequenceChecker::new(ssrc, initial_seq),
                        rtcp_handler: rtcp::TimestampPrinter::new(),
                    };
                },
                StreamState::Uninit => {},
                StreamState::Playing{..} => unreachable!(),
            };
        }
        Ok(Session {
            conn: self.conn,
            state: Playing {
                presentation: self.state.presentation,
                session_id,
                channels: self.state.channels,
                keepalive_timer: tokio::time::sleep(KEEPALIVE_DURATION),
                pending_keepalive_cseq: None,
            },
        })
    }
}

pub enum PacketItem {
    RtpPacket(rtp::Packet),
    SenderReport(rtp::SenderReport),
}

impl Session<Playing> {
    /// Returns a stream of packets.
    pub fn pkts(self) -> impl futures::Stream<Item = Result<PacketItem, Error>> {
        try_stream! {
            let self_ = self;
            tokio::pin!(self_);
            while let Some(pkt) = self_.as_mut().next().await {
                let pkt = pkt?;
                yield pkt;
            }
        }
    }

    pub fn demuxed(mut self) -> Result<impl futures::Stream<Item = Result<CodecItem, Error>>, Error> {
        for s in &mut self.state.presentation.streams {
            if matches!(s.state, StreamState::Playing{..}) {
                if let Err(ref mut e) = s.demuxer {
                    return Err(std::mem::replace(e, format_err!("(placeholder)")));
                }
            }
        }
        Ok(try_stream! {
            let self_ = self;
            tokio::pin!(self_);
            while let Some(pkt) = self_.as_mut().next().await {
                let pkt = pkt?;
                match pkt {
                    PacketItem::RtpPacket(p) => {
                        let self_ = self_.as_mut().project();
                        let state = self_.state.project();
                        let demuxer = match &mut state.presentation.streams[p.stream_id].demuxer {
                            Ok(d) => d,
                            Err(_) => unreachable!("demuxer was Ok"),
                        };
                        demuxer.push(p)?;
                        while let Some(demuxed) = demuxer.pull()? {
                            yield demuxed;
                        }
                    },
                    PacketItem::SenderReport(p) => yield CodecItem::SenderReport(p),
                };
            }
        })
    }

    /// Returns the next packet, an error, or `None` on end of stream.
    /// Also manages keepalives; this will send them as necessary to keep the
    /// stream open, and fail when sending a following keepalive if the
    /// previous one was never acknowledged.
    ///
    /// TODO: this should also pass along RTCP packets. There can be multiple
    /// RTCP packets per data message, so that will require keeping more state.
    async fn next(self: Pin<&mut Self>) -> Option<Result<PacketItem, Error>> {
        let this = self.project();
        let mut state = this.state.project();
        loop {
            tokio::select! {
                // Prefer receiving data to sending keepalives. If we can't keep
                // up with the server's data stream, it probably should drop us.
                biased;

                msg = this.conn.stream.next() => {
                    let msg = match msg {
                        Some(Ok(m)) => m,
                        Some(Err(e)) => return Some(Err(e)),
                        None => return None,
                    };
                    match msg.msg {
                        rtsp_types::Message::Data(data) => {
                            match Session::handle_data(&mut state, msg.ctx, data) {
                                Err(e) => return Some(Err(e)),
                                Ok(Some(pkt)) => return Some(Ok(pkt)),
                                Ok(None) => continue,
                            };
                        },
                        rtsp_types::Message::Response(response) => {
                            if let Err(e) = Session::handle_response(&mut state, response) {
                                return Some(Err(e));
                            }
                        },
                        rtsp_types::Message::Request(request) => {
                            warn!("Received RTSP request in Playing state. Responding unimplemented.\n{:#?}",
                                request);
                        },
                    }
                },

                () = &mut state.keepalive_timer => {
                    // TODO: deadlock possibility. Once we decide to send a
                    // keepalive, we don't try receiving anything until the
                    // keepalive is fully sent. The server might similarly be
                    // stubbornly trying to send before receiving. If all the
                    // socket buffers are full, deadlock can result.
                    //
                    // This is really unlikely right now when all we send are
                    // keepalives, which are probably much smaller than our send
                    // buffer. But if we start supporting ONVIF backchannel, it
                    // will become more of a concern.
                    if let Err(e) = Session::handle_keepalive_timer(this.conn, &mut state).await {
                        return Some(Err(e));
                    }
                },
            }
        }
    }

    async fn handle_keepalive_timer(conn: &mut RtspConnection, state: &mut PlayingProj<'_>) -> Result<(), Error> {
        // Check on the previous keepalive request.
        if let Some(cseq) = state.pending_keepalive_cseq {
            bail!("Server failed to respond to keepalive {} within {:?}", cseq, KEEPALIVE_DURATION);
        }

        // Send a new one and reset the timer.
        *state.pending_keepalive_cseq = Some(conn.send_nowait(
            &mut rtsp_types::Request::builder(rtsp_types::Method::GetParameter, rtsp_types::Version::V1_0)
            .request_uri(state.presentation.base_url.clone())
            .header(rtsp_types::headers::SESSION, state.session_id.clone())
            .build(Bytes::new())).await?);
        state.keepalive_timer.as_mut().reset(tokio::time::Instant::now() + KEEPALIVE_DURATION);
        Ok(())
    }

    fn handle_response(state: &mut PlayingProj<'_>, response: rtsp_types::Response<Bytes>) -> Result<(), Error> {
        if matches!(*state.pending_keepalive_cseq,
                    Some(cseq) if parse::get_cseq(&response) == Some(cseq)) {
            // We don't care if the keepalive response succeeds or fails. Just mark complete.
            *state.pending_keepalive_cseq = None;
            return Ok(())
        }

        // The only response we expect in this state is to our keepalive request.
        bail!("Unexpected RTSP response {:#?}", response);
    }

    fn handle_data(state: &mut PlayingProj<'_>, ctx: Context, data: rtsp_types::Data<Bytes>)
                   -> Result<Option<PacketItem>, Error> {
        let c = data.channel_id();
        let m = match state.channels.lookup(c) {
            Some(m) => m,
            None => bail!("Data message on unexpected channel {} at {:#?}", c, &ctx),
        };
        let stream = &mut state.presentation.streams[m.stream_i];
        let (mut timeline, rtp_handler, rtcp_handler) = match &mut stream.state {
            StreamState::Playing{timeline, rtp_handler, rtcp_handler} => {
                (timeline, rtp_handler, rtcp_handler)
            },
            _ => unreachable!("Session<Playing>'s {}->{:?} not in Playing state", c, m),
        };
        match m.channel_type {
            ChannelType::Rtp => Ok(Some(PacketItem::RtpPacket(
                rtp_handler.process(ctx, &mut timeline, m.stream_i, data.into_body())?))),
            ChannelType::Rtcp => {
                // TODO: pass RTCP packets along. Currenly this just logs them.
                // There can be multiple packets per data message, so we'll need to
                // keep Stream state.
                rtcp_handler.data(ctx, &mut timeline, data.into_body())?;
                Ok(None)
            },
        }
    }

    pub fn streams(&self) -> &[Stream] { &self.state.presentation.streams }
}

#[cfg(test)]
mod tests {
    use crate::client::{ChannelMapping, ChannelType};

    use super::Timeline;

    #[test]
    fn channel_mappings() {
        let mut mappings = super::ChannelMappings::default();
        assert_eq!(mappings.next_unassigned().unwrap(), 0);
        assert_eq!(mappings.lookup(0), None);
        mappings.assign(0, 42).unwrap();
        mappings.assign(0, 43).unwrap_err();
        mappings.assign(1, 43).unwrap_err();
        assert_eq!(mappings.lookup(0), Some(ChannelMapping {
            stream_i: 42,
            channel_type: ChannelType::Rtp,
        }));
        assert_eq!(mappings.lookup(1), Some(ChannelMapping {
            stream_i: 42,
            channel_type: ChannelType::Rtcp,
        }));
        assert_eq!(mappings.next_unassigned().unwrap(), 2);
        mappings.assign(9, 26).unwrap_err();
        mappings.assign(8, 26).unwrap();
        assert_eq!(mappings.lookup(8), Some(ChannelMapping {
            stream_i: 26,
            channel_type: ChannelType::Rtp,
        }));
        assert_eq!(mappings.lookup(9), Some(ChannelMapping {
            stream_i: 26,
            channel_type: ChannelType::Rtcp,
        }));
        assert_eq!(mappings.next_unassigned().unwrap(), 2);
    }

    #[test]
    fn timeline() {
        // Don't allow crazy clock rates that will get us into trouble.
        Timeline::new(Some(0), 0).unwrap_err();
        Timeline::new(Some(0), u32::MAX).unwrap_err();

        // Don't allow excessive forward jumps.
        let mut t = Timeline::new(Some(100), 90_000).unwrap();
        t.advance_to(100 + (super::MAX_FORWARD_TIME_JUMP_SECS * 90_000) + 1).unwrap_err();

        // Or any backward jump.
        let mut t = Timeline::new(Some(100), 90_000).unwrap();
        t.advance_to(99).unwrap_err();

        // Normal usage.
        let mut t = Timeline::new(Some(42), 90_000).unwrap();
        assert_eq!(t.advance_to(83).unwrap().elapsed(), 83 - 42);
        assert_eq!(t.advance_to(453).unwrap().elapsed(), 453 - 42);

        // Wraparound is normal too.
        let mut t = Timeline::new(Some(u32::MAX), 90_000).unwrap();
        assert_eq!(t.advance_to(5).unwrap().elapsed(), 5 + 1);

        // No initial rtptime.
        let mut t = Timeline::new(None, 90_000).unwrap();
        assert_eq!(t.advance_to(218250000).unwrap().elapsed(), 0);
    }
}
