//! Starts a RTSP stream and logs/discards all the packets.

use bytes::Bytes;
use failure::{Error, bail, format_err};
use moonfire_rtsp::client::ChannelHandler;
use rtcp::packet::Packet;
use std::fmt::Write;
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

#[tokio::main]
async fn main() {
    if let Err(e) = main_inner().await {
        eprintln!("{}", prettify_failure(&e));
        std::process::exit(1);
    }
}

async fn main_inner() -> Result<(), Error> {
    let opt = Opt::from_args();
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
    let describe = dbg!(cli.describe(opt.url).await?);
    let video = describe.sdp.media_descriptions.first().expect("has a media description");
    assert_eq!(video.media_name.media, "video");  // TODO: not guaranteed this is first.
    let video_control_url_str = video.attribute("control").expect("has control attribute");
    let video_control_url = describe.base_url.join(video_control_url_str).unwrap();

    // SETUP. https://tools.ietf.org/html/rfc2326#section-10.4
    let setup_resp = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Setup, rtsp_types::Version::V1_0)
        .request_uri(video_control_url)
        .header(rtsp_types::headers::TRANSPORT, "RTP/AVP/TCP;unicast;interleaved=0-1".to_owned())
        .header(moonfire_rtsp::X_DYNAMIC_RATE.clone(), "1".to_owned())
        .build(Bytes::new())).await?;
    dbg!(&setup_resp);
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
            let mut keyvalue = part.splitn(2, '=');
            let (key, value) = (keyvalue.next().unwrap(), keyvalue.next().unwrap());
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
    let mut nop_au = moonfire_rtsp::client::h264::NopAccessUnitHandler;
    let mut h264 = moonfire_rtsp::client::h264::Handler::new(&mut nop_au);
    let mut rtp = moonfire_rtsp::client::rtp::StrictSequenceChecker::new(video_ssrc, video_seq, video_rtptime, 90_000, &mut h264);
    let mut prev_sr: Option<rtcp::sender_report::SenderReport> = None;
    //let mut prev_rtcp: Option<rtcp::packet::Packet> = None;
    let mut timeout = tokio::time::Instant::now() + KEEPALIVE_DURATION;

    loop {
        tokio::select! {
            msg = cli.next() => {
                let msg = msg.ok_or_else(|| format_err!("EOF"))??;
                match msg.msg {
                    rtsp_types::Message::Data(data) => {
                        let channel = data.channel_id();
                        if channel == 0 {
                            rtp.data(msg.ctx, data.into_body())?;
                        } else if channel == 1 {
                            let mut body = data.into_body();
                            while !body.is_empty() {
                                let h = rtcp::header::Header::unmarshal(&body)?;
                                let pkt_len = (usize::from(h.length) + 1) * 4;
                                if pkt_len > body.len() {
                                    bail!("rtcp pkt len {} vs remaining body len {} at {:#?}", pkt_len, body.len(), &msg.ctx);
                                }
                                let pkt = body.split_to(pkt_len);
                                if h.packet_type == rtcp::header::PacketType::SenderReport {
                                    let pkt = rtcp::sender_report::SenderReport::unmarshal(&pkt)?;
                                    println!("rtcp sender report, ts={:20} ntp={:20}", pkt.rtp_time, pkt.ntp_time);
                                    if let Some(prev) = prev_sr.as_ref() {
                                        if pkt.rtp_time < prev.rtp_time {
                                            println!("sender report time went backwards. got {:#?} then {:#?} at {:#?}", &prev, &pkt, &msg.ctx);
                                        }
                                    }
                                    prev_sr = Some(pkt);
                                } else if h.packet_type == rtcp::header::PacketType::SourceDescription {
                                    let _pkt = rtcp::source_description::SourceDescription::unmarshal(&pkt)?;
                                    //println!("rtcp source description: {:#?}", &pkt);
                                } else {
                                    println!("rtcp: {:?}", h.packet_type);
                                }
                            }
                            //if let Some(prev_rtcp) = prev.as_ref() {
                            //}
                            //prev_rtcp = Some(pkt);
                        }
                    },
                    o => println!("message {:#?}", &o),
                }
            },
            _ = tokio::time::sleep_until(timeout) => {
                cli.send_nowait(
                    &mut rtsp_types::Request::builder(rtsp_types::Method::GetParameter, rtsp_types::Version::V1_0)
                    .request_uri(describe.base_url.clone())
                    .header(rtsp_types::headers::SESSION, session_id.to_owned())
                    .build(Bytes::new())).await?;
                timeout = tokio::time::Instant::now() + KEEPALIVE_DURATION;
            }
        }
    }
}