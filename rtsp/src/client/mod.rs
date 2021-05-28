use std::{borrow::Cow, fmt::Debug, num::NonZeroU16, pin::Pin};

use async_stream::try_stream;
use bytes::Bytes;
use failure::{Error, bail, format_err};
use futures::{SinkExt, StreamExt};
use log::{debug, trace, warn};
use pin_project::pin_project;
use sdp::session_description::SessionDescription;
use self::channel_mapping::*;
use self::timeline::Timeline;
use tokio::pin;
use tokio_util::codec::Framed;
use url::Url;

use crate::{Context, codec::CodecItem};

mod channel_mapping;
mod parse;
mod timeline;
pub mod rtcp;
pub mod rtp;

/// Duration between keepalive RTSP requests during [Playing] state.
pub const KEEPALIVE_DURATION: std::time::Duration = std::time::Duration::from_secs(30);

/// Policy for handling the `rtptime` parameter normally seem in the `RTP-Info header.
/// This parameter is used to map each stream's RTP timestamp to NPT ("normal play time"),
/// allowing multiple streams to be played in sync.
#[derive(Copy, Clone, Debug)]
pub enum InitialTimestampPolicy {
    /// Default policy: currently `Require` when playing multiple streams,
    /// `Ignore` otherwise.
    Default,

    /// Require the `rtptime` parameter be present and use it to set NPT. Use
    /// when accurate multi-stream NPT is important.
    Require,

    /// Ignore the `rtptime` parameter and assume the first received packet for
    /// each stream is at NPT 0. Use with cameras that are known to set
    /// `rtptime` incorrectly.
    Ignore,

    /// Use the `rtptime` parameter when playing multiple streams if it's
    /// specified for all of them; otherwise assume the first received packet
    /// for each stream is at NPT 0.
    Permissive,
}

impl Default for InitialTimestampPolicy {
    fn default() -> Self {
        InitialTimestampPolicy::Default
    }
}

impl std::fmt::Display for InitialTimestampPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InitialTimestampPolicy::Default => f.pad("default"),
            InitialTimestampPolicy::Require => f.pad("require"),
            InitialTimestampPolicy::Ignore => f.pad("ignore"),
            InitialTimestampPolicy::Permissive => f.pad("permissive"),
        }
    }
}

impl std::str::FromStr for InitialTimestampPolicy {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "default" => InitialTimestampPolicy::Default,
            "require" => InitialTimestampPolicy::Require,
            "ignore" => InitialTimestampPolicy::Ignore,
            "permissive" => InitialTimestampPolicy::Permissive,
            _ => bail!("Initial timestamp mode {:?} not understood", s),
        })
    }
}

/// Adjustments to make on `PLAY` for non-compliant server implementations.
#[derive(Default)]
pub struct PlayQuirks {
    initial_timestamp: InitialTimestampPolicy,
    ignore_zero_seq: bool,
}

impl PlayQuirks {
    pub fn initial_timestamp(self, initial_timestamp: InitialTimestampPolicy) -> Self {
        Self {
            initial_timestamp,
            ..self
        }
    }

    /// If the `RTP-Time` specifies `seq=0`, ignore it. Some cameras set this value then start
    /// the stream with something dramatically different. (Eg the Hikvision DS-2CD2032-I on its
    /// metadata stream; the other streams are fine.)
    pub fn ignore_zero_seq(self, ignore_zero_seq: bool) -> Self {
        Self {
            ignore_zero_seq,
            ..self
        }
    }
}

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

pub struct Credentials {
    pub username: String,
    pub password: String,
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
        let conn_established_wall = time::get_time();
        let conn_established = std::time::Instant::now();
        let conn_local_addr = stream.local_addr()?;
        let conn_peer_addr = stream.peer_addr()?;
        let stream = Framed::new(stream, crate::Codec {
            ctx: crate::Context {
                conn_established_wall,
                conn_established,
                conn_local_addr,
                conn_peer_addr,
                msg_pos: 0,
                msg_received_wall: conn_established_wall,
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
    pub async fn play(mut self, quirks: PlayQuirks) -> Result<Session<Playing>, Error> {
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

        let all_have_time = self.state.presentation.streams.iter().all(|s| match s.state {
            StreamState::Init(StreamStateInit { initial_rtptime, .. }) => initial_rtptime.is_some(),
            _ => true,
        });

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
                    let initial_rtptime = match quirks.initial_timestamp {
                        InitialTimestampPolicy::Require
                        | InitialTimestampPolicy::Default if setup_streams > 1 => {
                            if initial_rtptime.is_none() {
                                bail!(
                                    "Expected rtptime on PLAY with mode {:?}, missing on stream \
                                     {} ({:?}). Consider setting initial timestamp mode \
                                     use-if-all-present.",
                                    quirks.initial_timestamp, i, &s.control);
                            }
                            initial_rtptime
                        },
                        InitialTimestampPolicy::Permissive if setup_streams > 1 && all_have_time => {
                            initial_rtptime
                        },
                        _ => None,
                    };
                    let initial_seq = match initial_seq {
                        Some(0) if quirks.ignore_zero_seq => {
                            log::info!("Ignoring seq=0 on stream {}", i);
                            None
                        },
                        o => o,
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
