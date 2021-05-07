use std::{fmt::Debug, num::NonZeroU8};

use async_trait::async_trait;
use bytes::Bytes;
use failure::{Error, bail, format_err};
use futures::{SinkExt, StreamExt};
use log::{debug, trace};
use sdp::session_description::SessionDescription;
use tokio_util::codec::Framed;
use url::Url;

pub mod application;
mod parse;
pub mod rtcp;
pub mod rtp;
pub mod video;

const MAX_TS_JUMP_SECS: u32 = 10;

/// Handles data from a RTSP data channel.
#[async_trait]
pub trait ChannelHandler {
    async fn data(&mut self, ctx: crate::Context, timeline: &mut Timeline, data: Bytes) -> Result<(), Error>;
    async fn end(&mut self) -> Result<(), Error>;
}

#[derive(Debug)]
pub struct Presentation {
    pub streams: Vec<Stream>,
    pub base_url: Url,
    pub control: String,
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
    /// registry](https://www.iana.org/assignments/media-types/media-types.xhtml).
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

    /// The metadata, if of a known codec type.
    /// Currently the only supported codec is H.264. This will be extended to
    /// be an enum or something.
    pub metadata: Option<crate::client::video::h264::Metadata>,

    /// The specified control URL.
    /// This is needed to send `SETUP` requests and interpret the `PLAY`
    /// response's `RTP-Info` header.
    pub control: String,

    /// The RTP synchronization source (SSRC), as defined in
    /// [RFC 3550](https://tools.ietf.org/html/rfc3550). This is normally
    /// supplied in the `SETUP` response's `Transport` header. Reolink cameras
    /// instead supply it in the `PLAY` response's `RTP-Info` header.
    pub ssrc: Option<u32>,

    /// The initial RTP sequence number, as specified in the `PLAY` response's
    /// `RTP-Info` header.
    pub initial_seq: Option<u16>,

    /// The initial RTP timestamp, as specified in the `PLAY` response's
    /// `RTP-Info` header.
    pub initial_rtptime: Option<u32>,

    state: StreamState,
}

#[derive(Debug)]
enum StreamState {
    /// Uninitialized; no `SETUP` has yet been sent.
    Uninit,

    /// `SETUP` reply has been received.
    Init,
}

#[derive(Debug)]
pub struct Timeline {
    latest: crate::Timestamp,
    max_jump: u32,
}

impl Timeline {
    pub fn new(start: u32, clock_rate: u32) -> Self {
        Timeline {
            latest: crate::Timestamp {
                timestamp: u64::from(start),
                start,
                clock_rate,
            },
            max_jump: MAX_TS_JUMP_SECS * clock_rate,
        }
    }

    fn advance(&mut self, rtp_timestamp: u32) -> Result<crate::Timestamp, Error> {
        // TODO: error on u64 overflow.
        let ts_high_bits = self.latest.timestamp & 0xFFFF_FFFF_0000_0000;
        let new_ts = match rtp_timestamp < (self.latest.timestamp as u32) {
            true  => ts_high_bits + 1u64<<32 + u64::from(rtp_timestamp),
            false => ts_high_bits + u64::from(rtp_timestamp),
        };
        let forward_ts = crate::Timestamp {
            timestamp: new_ts,
            clock_rate: self.latest.clock_rate,
            start: self.latest.start,
        };
        let forward_delta = forward_ts.timestamp - self.latest.timestamp;
        if forward_delta > u64::from(self.max_jump) {
            let backward_ts = crate::Timestamp {
                timestamp: ts_high_bits + (self.latest.timestamp & 0xFFFF_FFFF) - u64::from(rtp_timestamp),
                clock_rate: self.latest.clock_rate,
                start: self.latest.start,
            };
            bail!("Timestamp jumped (forward by {} from {} to {}, more than allowed {} sec OR backward by {} from {} to {})",
                  forward_delta, self.latest.timestamp, new_ts, MAX_TS_JUMP_SECS,
                  self.latest.timestamp - backward_ts.timestamp, self.latest.timestamp, backward_ts);
        }
        self.latest = forward_ts;
        Ok(self.latest)
    }
}


pub struct Credentials {
    pub username: String,
    pub password: String,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChannelType {
    Rtp,
    Rtcp,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ChannelMapping {
    pub stream_i: usize,
    pub channel_type: ChannelType,
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
/// These assumptions let us do the full mapping in 128 bytes with a trivial
/// lookup.
#[repr(transparent)]
#[derive(Copy, Clone)]
struct ChannelMappings([Option<NonZeroU8>; 128]);

impl ChannelMappings {
    /// Creates an empty mapping.
    fn new() -> Self {
        Self([None; 128])
    }

    /// Returns the next unassigned even channel id, or errors.
    fn next_unassigned(&self) -> Result<u8, Error> {
        self.0.iter().position(|c| c.is_none())
            .map(|c| (c as u8) << 1)
            .ok_or_else(|| format_err!("all RTSP channels have been assigned"))
    }

    /// Assigns an even channel id (to RTP) and its odd successor (to RTCP) or errors.
    fn assign(&mut self, channel_id: u8, stream_i: usize) -> Result<(), Error> {
        if (channel_id & 1) != 0 {
            bail!("Can't assign odd channel id {}", channel_id);
        }
        if stream_i >= 255 {
            bail!("Can't assign channel to stream id {} because it's >= 255", stream_i);
        }
        let c = &mut self.0[usize::from(channel_id >> 1)];
        if let Some(c) = c {
            bail!("Channel id {} is already assigned to stream {}; won't reassign to stream {}",
                  channel_id, c.get() - 1, channel_id);
        }
        *c = Some(NonZeroU8::new((stream_i + 1) as u8).expect("[0, 255) + 1 is non-zero"));
        Ok(())
    }

    /// Looks up a channel id's mapping.
    fn lookup(&self, channel_id: u8) -> Option<ChannelMapping> {
        self.0[usize::from(channel_id >> 1)].map(|c| ChannelMapping {
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
                v
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
pub struct Playing {
    presentation: Presentation,
    session_id: String,
    channels: ChannelMappings,
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
pub struct Session<S: State> {
    conn: RtspConnection,
    state: S,
}

/// Converts from an RTSP method to a digest method.
/// Unfortunately all [digest_auth::HttpMethod]s have to be `&'static`, where all the other parameters are `Cow`.
/// Therefore extension methods aren't supported for now.
fn http_method(method: &rtsp_types::Method) -> Result<digest_auth::HttpMethod, Error> {
    use rtsp_types::Method;
    Ok(digest_auth::HttpMethod::OTHER(match method {
        Method::Describe => "DESCRIBE",
        Method::GetParameter => "GET_PARAMETER",
        Method::Options => "OPTIONS",
        Method::Pause => "PAUSE",
        Method::Play => "PLAY",
        Method::PlayNotify => "PLAY_NOTIFY",
        Method::Redirect => "REDIRECT",
        Method::Setup => "SETUP",
        Method::SetParameter => "SET_PARAMETER",
        Method::Teardown => "TEARDOWN",
        Method::Extension(m) => bail!("can't authenticate with method {:?}", &m),
    }))
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
        let established = std::time::SystemTime::now();
        let local_addr = stream.local_addr()?;
        let peer_addr = stream.peer_addr()?;
        let stream = Framed::new(stream, crate::Codec {
            ctx: crate::Context {
                established,
                local_addr,
                peer_addr,
                rtsp_message_offset: 0,
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
    /// Takes care of authorization and `C-Seq`. Returns `Error` if not successful.
    async fn send(&mut self, req: &mut rtsp_types::Request<Bytes>) -> Result<rtsp_types::Response<Bytes>, Error> {
        loop {
            let cseq = self.send_nowait(req).await?;
            let msg = self.stream.next().await.ok_or_else(|| format_err!("unexpected EOF while waiting for reply"))??;
            let resp = match msg.msg {
                rtsp_types::Message::Response(r) => r,
                o => bail!("Unexpected RTSP message {:?}", &o),
            };
            if !matches!(resp.header(&rtsp_types::headers::CSEQ), Some(v) if v.as_str() == &cseq[..]) {
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

    /// Sends a request without waiting for a response, returning the `CSeq` as a string.
    async fn send_nowait(&mut self, req: &mut rtsp_types::Request<Bytes>) -> Result<String, Error> {
        let cseq = self.next_cseq.to_string();
        self.next_cseq += 1;
        match (self.requested_auth.as_mut(), self.creds.as_ref()) {
            (None, _) => {},
            (Some(auth), Some(creds)) => {
                let uri = req.request_uri().map(|u| u.as_str()).unwrap_or("*");
                let ctx = digest_auth::AuthContext::new_with_method(
                    &creds.username, &creds.password, uri, Option::<&'static [u8]>::None, http_method(req.method())?);
                let authorization = auth.respond(&ctx)?.to_string();
                req.insert_header(rtsp_types::headers::AUTHORIZATION, authorization);
            },
            (Some(_), None) => bail!("Authentication required; no credentials supplied"),
        }
        req.insert_header(rtsp_types::headers::CSEQ, cseq.clone());
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
                channels: ChannelMappings::new(),
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
        let response = self.conn.send(
            &mut rtsp_types::Request::builder(rtsp_types::Method::Setup, rtsp_types::Version::V1_0)
            .request_uri(parse::join_control(&self.state.presentation.base_url, &stream.control)?)
            .header(
                rtsp_types::headers::TRANSPORT,
                format!(
                    "RTP/AVP/TCP;unicast;interleaved={}-{}",
                    proposed_channel_id,
                    proposed_channel_id + 1)
            )
            .header(crate::X_DYNAMIC_RATE.clone(), "1".to_owned())
            .build(Bytes::new())).await?;
        debug!("SETUP response: {:#?}", &response);
        let channel_id = parse::parse_setup(response, &mut self.state.session_id, stream)?;
        self.state.channels.assign(channel_id, stream_i)?;
        stream.state = StreamState::Init;
        Ok(())
    }

    /// Sends a `PLAY` request for the presentation.
    /// The presentation must support aggregate control, as defined in [RFC 2326
    /// section 1.3](https://tools.ietf.org/html/rfc2326#section-1.3).
    pub async fn play(mut self) -> Result<Session<Playing>, Error> {
        let session_id = self.state.session_id.take().ok_or_else(|| format_err!("must SETUP before PLAY"))?;
        trace!("PLAY with channel mappings: {:#?}", &self.state.channels);
        let response = self.conn.send(
            &mut rtsp_types::Request::builder(rtsp_types::Method::Play, rtsp_types::Version::V1_0)
            .request_uri(parse::join_control(&self.state.presentation.base_url, &self.state.presentation.control)?)
            .header(rtsp_types::headers::SESSION, session_id.clone())
            .header(rtsp_types::headers::RANGE, "npt=0.000-".to_owned())
            .build(Bytes::new())).await?;
        parse::parse_play(response, &mut self.state.presentation)?;
        Ok(Session {
            conn: self.conn,
            state: Playing {
                presentation: self.state.presentation,
                session_id,
                channels: self.state.channels,
            },
        })
    }
}

impl Session<Playing> {
    pub async fn next(&mut self) -> Option<Result<crate::ReceivedMessage, Error>> {
        self.conn.stream.next().await
    }

    pub fn streams(&self) -> &[Stream] { &self.state.presentation.streams }

    pub fn channel(&self, channel_id: u8) -> Option<ChannelMapping> {
        self.state.channels.lookup(channel_id)
    }

    pub async fn send_keepalive(&mut self) -> Result<(), Error> {
        self.conn.send_nowait(
            &mut rtsp_types::Request::builder(rtsp_types::Method::GetParameter, rtsp_types::Version::V1_0)
            .request_uri(self.state.presentation.base_url.clone())
            .header(rtsp_types::headers::SESSION, self.state.session_id.clone())
            .build(Bytes::new())).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::client::{ChannelMapping, ChannelType};

    #[test]
    fn channel_mappings() {
        let mut mappings = super::ChannelMappings::new();
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
}
