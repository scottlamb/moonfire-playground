use async_trait::async_trait;
use bytes::Bytes;
use failure::{Error, format_err};
use log::info;
use moonfire_rtsp::client::{application::onvif::MessageHandler, rtp::PacketHandler};
use rtsp_types::Url;

struct MessagePrinter;

#[async_trait]
impl MessageHandler for MessagePrinter {
    async fn message(&mut self, timestamp: moonfire_rtsp::Timestamp, msg: Bytes) -> Result<(), failure::Error> {
        info!("{}: {}\n", &timestamp, std::str::from_utf8(&msg[..]).unwrap());
        Ok(())
    }
}

pub async fn run(url: Url, credentials: Option<moonfire_rtsp::client::Credentials>) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c();

    let mut session = moonfire_rtsp::client::Session::describe(url, credentials).await?;
    let onvif_stream_i = session.streams().iter()
        .position(|s| s.media == "application" && s.encoding_name == "vnd.onvif.metadata")
        .ok_or_else(|| format_err!("couldn't find onvif stream"))?;
    session.setup(onvif_stream_i).await?;
    let session = session.play().await?;

    // Read RTP data.
    let mut onvifer = moonfire_rtsp::client::application::onvif::Handler::new(MessagePrinter);
    tokio::pin!(session);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            pkt = session.as_mut().next() => {
                let pkt = pkt.ok_or_else(|| format_err!("EOF"))??;
                onvifer.pkt(pkt).await?;
            },
            _ = &mut stop => {
                break;
            },
        }
    }
    Ok(())
}
