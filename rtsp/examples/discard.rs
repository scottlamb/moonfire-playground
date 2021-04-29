//! Starts a RTSP stream and logs/discards all the packets.

use bytes::Bytes;
use failure::{Error, format_err};
use rtp::packetizer::Marshaller;
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

#[tokio::main]
async fn main() -> Result<(), Error> {
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
    let media = describe.sdp.media_descriptions.first().expect("has a media description");
    assert_eq!(media.media_name.media, "video");
    let media_url = describe.base_url.join(media.attribute("control").expect("has control attribute"))?;

    // SETUP. https://tools.ietf.org/html/rfc2326#section-10.4
    let setup_resp = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Setup, rtsp_types::Version::V1_0)
        .request_uri(media_url)
        .header(rtsp_types::headers::TRANSPORT, "RTP/AVP/TCP;unicast;interleaved=0-1".to_owned())
        .header(moonfire_rtsp::X_DYNAMIC_RATE.clone(), "1".to_owned())
        .build(Bytes::new())).await?;
    dbg!(&setup_resp);
    let session = setup_resp.header(&rtsp_types::headers::SESSION).expect("has session");
    let session_id = session.as_str().split(';').next().expect("has session id");

    // PLAY. https://tools.ietf.org/html/rfc2326#section-10.5
    let play_resp = cli.send(
        &mut rtsp_types::Request::builder(rtsp_types::Method::Play, rtsp_types::Version::V1_0)
        .request_uri(describe.base_url.clone())
        .header(rtsp_types::headers::SESSION, session_id.to_owned())
        .header(rtsp_types::headers::RANGE, "npt=0.000-".to_owned())
        .build(Bytes::new())).await?;
    dbg!(&play_resp);

    // Read RTP data.
    let mut prev_by_channel: [Option<rtp::packet::Packet>; 2] = [None, None];
    let mut timeout = tokio::time::Instant::now() + KEEPALIVE_DURATION;

    loop {
        tokio::select! {
            msg = cli.next() => {
                match msg.ok_or_else(|| format_err!("EOF"))?? {
                    rtsp_types::Message::Data(data) => {
                        let channel = data.channel_id();
                        let pkt = rtp::packet::Packet::unmarshal(&data.into_body())?;
                        println!("pkt: channel={} sequence_number={:10} timestamp={:20}", channel, pkt.header.sequence_number, pkt.header.timestamp);
                        let prev = &mut prev_by_channel[usize::from(channel)];
                        if let Some(prev) = prev.as_ref() {
                            if pkt.header.sequence_number == prev.header.sequence_number {
                                println!("duplicate sequence number: got {:#?} then {:#?}", &prev, &pkt);
                            } else if pkt.header.sequence_number != prev.header.sequence_number.wrapping_add(1) {
                                println!("out of sequence: got {:#?} then {:#?}", &prev, &pkt);
                            }
                            if pkt.header.timestamp < prev.header.timestamp {
                                println!("timestamps non-increasing: got {:#?} then {:#?}", &prev.header, &pkt.header);
                            }
                        }
                        *prev = Some(pkt);
                    },
                    o => panic!("unexpected message {:#?}", &o),
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