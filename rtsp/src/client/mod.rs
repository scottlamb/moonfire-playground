use async_trait::async_trait;
use bytes::Bytes;
use failure::{Error, bail, format_err};
use futures::{SinkExt, StreamExt};
use sdp::session_description::SessionDescription;
use tokio_util::codec::Framed;
use url::Url;

mod parse;
pub mod rtcp;
pub mod rtp;
pub mod video;

pub use parse::{Presentation, Stream, join_control, parse_play, parse_setup};

pub struct Credentials {
    pub username: String,
    pub password: String,
}

pub struct Session {
    creds: Option<Credentials>,
    requested_auth: Option<digest_auth::WwwAuthenticateHeader>,
    stream: Framed<tokio::net::TcpStream, crate::Codec>,
    user_agent: String,
    cseq: u32,
    //session_id: Option<String>,
}

/// Handles data from a RTSP data channel.
#[async_trait]
pub trait ChannelHandler {
    async fn data(&mut self, ctx: crate::Context, timeline: &mut crate::Timeline, data: Bytes) -> Result<(), Error>;
    async fn end(&mut self) -> Result<(), Error>;
}

#[derive(Debug)]
pub struct DescribeResponse {
    /// True iff `X-Accept-Dynamic-Rate: 1` is set.
    pub accept_dynamic_rate: bool,

    /// The `Content-Base`, `Content-Location`, or request URL, as specified in RFC 2326 section C.1.1.
    pub base_url: Url,

    pub sdp: SessionDescription,
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

impl Session {
    pub async fn connect(url: &Url, creds: Option<Credentials>) -> Result<Self, Error> {
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
        Ok(Session {
            creds,
            requested_auth: None,
            stream,
            user_agent: "moonfire-rtsp test".to_string(),
            cseq: 1,
            //session_id: None,
        })
    }

    /// Sends a request and expects the next message from the peer to be its response.
    /// Takes care of authorization and `C-Seq`. Returns `Error` if not successful.
    pub async fn send(&mut self, req: &mut rtsp_types::Request<Bytes>) -> Result<rtsp_types::Response<Bytes>, Error> {
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
    pub async fn send_nowait(&mut self, req: &mut rtsp_types::Request<Bytes>) -> Result<String, Error> {
        let cseq = self.cseq.to_string();
        self.cseq += 1;
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

    pub async fn describe(&mut self, url: Url) -> Result<Presentation, Error> {
        let mut req = rtsp_types::Request::builder(rtsp_types::Method::Describe, rtsp_types::Version::V1_0)
            .header(rtsp_types::headers::ACCEPT, "application/sdp")
            .request_uri(url.clone())
            .build(Bytes::new());
        let resp = self.send(&mut req).await?;
        parse::parse_describe(url, resp)
    }

    pub async fn next(&mut self) -> Option<Result<crate::ReceivedMessage, Error>> {
        self.stream.next().await
    }
}
