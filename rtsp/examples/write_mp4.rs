//! Starts a RTSP stream and logs/discards all the packets.

use bytes::{Buf, Bytes};
use failure::{Error, bail, format_err};
use log::{debug, error, info, log_enabled, trace};
use moonfire_rtsp::client::ChannelHandler;
use moonfire_rtsp::client::video::h264;
use std::{fmt::Write, path::PathBuf, str::FromStr};
use structopt::StructOpt;

const KEEPALIVE_DURATION: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(StructOpt)]
struct Opt {
    #[structopt(long, parse(try_from_str))]
    url: url::Url,

    #[structopt(long, parse(try_from_str))]
    username: String,

    #[structopt(long, parse(try_from_str))]
    password: String,

    #[structopt(long, parse(try_from_str))]
    out: PathBuf,
}

/// Returns a pretty-and-informative version of `e`.
pub fn prettify_failure(e: &failure::Error) -> String {
    let mut msg = e.to_string();
    for cause in e.iter_causes() {
        write!(&mut msg, "\ncaused by: {}", cause).unwrap();
    }
    if e.backtrace().is_empty() {
        write!(
            &mut msg,
            "\n\n(set environment variable RUST_BACKTRACE=1 to see backtraces)"
        )
        .unwrap();
    } else {
        write!(&mut msg, "\n\nBacktrace:\n{}", e.backtrace()).unwrap();
    }
    msg
}

fn init_logging() -> mylog::Handle {
    let h = mylog::Builder::new()
        .set_format(::std::env::var("MOONFIRE_FORMAT")
                    .map_err(|_| ())
                    .and_then(|s| mylog::Format::from_str(&s))
                    .unwrap_or(mylog::Format::Google))
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
        .build();
    h.clone().install().unwrap();
    h
}

#[tokio::main]
async fn main() {
    let mut h = init_logging();
    let _a = h.async_scope();
    if let Err(e) = main_inner().await {
        error!("{}", prettify_failure(&e));
        std::process::exit(1);
    }
}

fn split_key_value(keyvalue: &str) -> Option<(&str, &str)> {
    keyvalue.find('=').map(|p| (&keyvalue[0..p], &keyvalue[p+1..]))
}

async fn main_inner() -> Result<(), Error> {
    let opt = Opt::from_args();
    let stop = tokio::signal::ctrl_c();
    let mut cli = moonfire_rtsp::client::Session::connect(&opt.url, Some(moonfire_rtsp::client::Credentials {
        username: opt.username,
        password: opt.password,
    })).await?;

    // OPTIONS. https://tools.ietf.org/html/rfc2326#section-10.1
    let _options = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Options, rtsp_types::Version::V1_0)
        .request_uri(opt.url.clone())
        .build(Bytes::new())).await?;

    // DESCRIBE. https://tools.ietf.org/html/rfc2326#section-10.2
    let describe = cli.describe(opt.url).await?;
    debug!("DESCRIBE response: {:#?}", &describe);
    let video = describe.sdp.media_descriptions.first().expect("has a media description");
    assert_eq!(video.media_name.media, "video");  // TODO: not guaranteed this is first.
    let video_control_url_str = video.attribute("control").expect("has control attribute");
    let video_fmtp = video.attribute("fmtp").expect("has fmtp");
    let video_fmtp_params = &video_fmtp[video_fmtp.find(' ').unwrap() + 1..];
    let mut video_metadata = None;
    for p in video_fmtp_params.split(';') {
        let (key, value) = split_key_value(p.trim()).unwrap();
        if key == "sprop-parameter-sets" {
            video_metadata = Some(h264::Metadata::from_sprop_parameter_sets(value)?);
        }
    }
    let video_metadata = video_metadata.unwrap();
    info!("video metadata: {:#?}", &video_metadata);

    let video_control_url = describe.base_url.join(video_control_url_str).unwrap();

    // SETUP. https://tools.ietf.org/html/rfc2326#section-10.4
    let setup_resp = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Setup, rtsp_types::Version::V1_0)
        .request_uri(video_control_url)
        .header(rtsp_types::headers::TRANSPORT, "RTP/AVP/TCP;unicast;interleaved=0-1".to_owned())
        .header(moonfire_rtsp::X_DYNAMIC_RATE.clone(), "1".to_owned())
        .build(Bytes::new())).await?;
    debug!("SETUP response: {:#?}", &setup_resp);
    let session = setup_resp.header(&rtsp_types::headers::SESSION).expect("has session");
    let session_id = session.as_str().split(';').next().expect("has session id");

    // The ssrc is supposed to be specified in the Transport header.
    // TODO: Reolink cameras actually specify it in the PLAY response's RTP-Info header instead.
    let transport = setup_resp.header(&rtsp_types::headers::TRANSPORT).expect("has Transport");
    let mut video_ssrc = None;
    for part in transport.as_str().split(';') {
        if part.starts_with("ssrc=") {
            video_ssrc = Some(u32::from_str_radix(&part["ssrc=".len()..], 16).unwrap());
            break;
        }
    }
    let video_ssrc = video_ssrc.unwrap();

    // PLAY. https://tools.ietf.org/html/rfc2326#section-10.5
    let play_resp = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Play, rtsp_types::Version::V1_0)
        .request_uri(describe.base_url.clone())
        .header(rtsp_types::headers::SESSION, session_id.to_owned())
        .header(rtsp_types::headers::RANGE, "npt=0.000-".to_owned())
        .build(Bytes::new())).await?;
    let rtp_info = play_resp.header(&rtsp_types::headers::RTP_INFO).expect("has RTP-Info");
    let mut video_seq = None;
    let mut video_rtptime = None;
    for stream in rtp_info.as_str().split(',') {
        let stream = stream.trim();
        let mut parts = stream.split(';');
        let url_part = parts.next().unwrap();
        assert!(url_part.starts_with("url="));
        if &url_part["url=".len()..] != video_control_url_str {
           continue;
        }
        for part in parts {
            let (key, value) = split_key_value(part).unwrap();
            match key {
                "seq" => video_seq = Some(u16::from_str_radix(value, 10).unwrap()),
                "rtptime" => video_rtptime = Some(u32::from_str_radix(value, 10).unwrap()),
                _ => {},
            }
         }
    }
    let video_seq = video_seq.unwrap();
    let video_rtptime = video_rtptime.unwrap();
    dbg!(&play_resp);

    // Read RTP data.
    let out = tokio::fs::File::create(opt.out).await?;
    let mp4_vid = moonfire_rtsp::mp4::Mp4Writer::new(video_metadata.clone(), out).await?;
    let to_vid = moonfire_rtsp::client::video::h264::VideoAccessUnitHandler::new(video_metadata, mp4_vid);
    //let mut print_au = moonfire_rtsp::client::video::h264::PrintAccessUnitHandler::new(&video_metadata)?;
    let mut h264_timeline = moonfire_rtsp::Timeline::new(video_rtptime, 90_000);
    let h264 = moonfire_rtsp::client::video::h264::Handler::new(to_vid);
    let mut h264_rtp = moonfire_rtsp::client::rtp::StrictSequenceChecker::new(video_ssrc, video_seq, h264);
    let mut h264_rtcp = moonfire_rtsp::client::rtcp::TimestampPrinter::new();

    let timeout = tokio::time::sleep(KEEPALIVE_DURATION);
    tokio::pin!(stop);
    tokio::pin!(timeout);
    loop {
        tokio::select! {
            msg = cli.next() => {
                let msg = msg.ok_or_else(|| format_err!("EOF"))??;
                trace!("msg: {:#?}", &msg);
                match msg.msg {
                    rtsp_types::Message::Data(data) => {
                        match data.channel_id() {
                            0 => h264_rtp.data(msg.ctx, &mut h264_timeline, data.into_body()).await?,
                            1 => h264_rtcp.data(msg.ctx, &mut h264_timeline, data.into_body()).await?,
                            o => bail!("Data message on unexpected channel {} at {:#?}", o, &msg.ctx),
                        }
                    },
                    o => println!("message {:#?}", &o),
                }
            },
            () = &mut timeout => {
                cli.send_nowait(
                    &mut rtsp_types::Request::builder(rtsp_types::Method::GetParameter, rtsp_types::Version::V1_0)
                    .request_uri(describe.base_url.clone())
                    .header(rtsp_types::headers::SESSION, session_id.to_owned())
                    .build(Bytes::new())).await?;
                timeout.as_mut().reset(tokio::time::Instant::now() + KEEPALIVE_DURATION);
            },
            _ = &mut stop => {
                break;
            },
        }
    }
    info!("Stopping");
    let mp4 = h264_rtp.into_inner().into_inner().into_inner();
    mp4.finish().await?;
    info!("Done");
    Ok(())
}
