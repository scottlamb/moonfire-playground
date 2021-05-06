//! Starts a RTSP stream and logs/discards all the packets.

use bytes::{Buf, Bytes};
use failure::{Error, bail, format_err};
use log::{debug, error, info, log_enabled, trace};
use moonfire_rtsp::client::{ChannelHandler, join_control};
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

    // DESCRIBE. https://tools.ietf.org/html/rfc2326#section-10.2
    let mut presentation = cli.describe(opt.url).await?;
    debug!("DESCRIBE response: {:#?}", &presentation);
    let video_stream_i = presentation.streams.iter()
        .position(|s| s.media == "video")
        .ok_or_else(|| format_err!("couldn't find video stream"))?;
    let video_metadata = presentation.streams[video_stream_i].metadata.as_ref().unwrap().clone();
    info!("video metadata: {:#?}", &video_metadata);

    // SETUP. https://tools.ietf.org/html/rfc2326#section-10.4
    let mut session_id = None;
    let setup_resp = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Setup, rtsp_types::Version::V1_0)
        .request_uri(join_control(&presentation.base_url, &presentation.streams[video_stream_i].control)?)
        .header(rtsp_types::headers::TRANSPORT, "RTP/AVP/TCP;unicast;interleaved=0-1".to_owned())
        .header(moonfire_rtsp::X_DYNAMIC_RATE.clone(), "1".to_owned())
        .build(Bytes::new())).await?;
    debug!("SETUP response: {:#?}", &setup_resp);
    moonfire_rtsp::client::parse_setup(
        setup_resp,
        &mut session_id,
        &mut presentation.streams[video_stream_i]
    )?;

    // PLAY. https://tools.ietf.org/html/rfc2326#section-10.5
    let play_resp = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Play, rtsp_types::Version::V1_0)
        .request_uri(join_control(&presentation.base_url, &presentation.control)?)
        .header(rtsp_types::headers::SESSION, session_id.as_deref().unwrap())
        .header(rtsp_types::headers::RANGE, "npt=0.000-".to_owned())
        .build(Bytes::new())).await?;
    moonfire_rtsp::client::parse_play(play_resp, &mut presentation)?;

    // Read RTP data.
    let out = tokio::fs::File::create(opt.out).await?;
    let mp4_vid = moonfire_rtsp::mp4::Mp4Writer::new(video_metadata.clone(), out).await?;
    let to_vid = moonfire_rtsp::client::video::h264::VideoAccessUnitHandler::new(video_metadata.clone(), mp4_vid);
    //let mut print_au = moonfire_rtsp::client::video::h264::PrintAccessUnitHandler::new(&video_metadata)?;
    let video_stream = &presentation.streams[video_stream_i];
    let mut h264_timeline = moonfire_rtsp::Timeline::new(video_stream.initial_rtptime.unwrap(), video_stream.clock_rate);
    let h264 = moonfire_rtsp::client::video::h264::Handler::new(to_vid);
    let mut h264_rtp = moonfire_rtsp::client::rtp::StrictSequenceChecker::new(
        video_stream.ssrc.unwrap(),
        video_stream.initial_seq.unwrap(),
        h264
    );
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
                    .request_uri(presentation.base_url.clone())
                    .header(rtsp_types::headers::SESSION, session_id.as_deref().unwrap())
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
