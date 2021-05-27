use failure::{Error, format_err};
use futures::StreamExt;
use log::info;
use moonfire_rtsp::codec::CodecItem;
use rtsp_types::Url;

pub async fn run(url: Url, credentials: Option<moonfire_rtsp::client::Credentials>) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c();

    let mut session = moonfire_rtsp::client::Session::describe(url, credentials).await?;
    let onvif_stream_i = session.streams().iter()
        .position(|s| matches!(s.parameters(), Some(moonfire_rtsp::codec::Parameters::Message(..))))
        .ok_or_else(|| format_err!("couldn't find onvif stream"))?;
    session.setup(onvif_stream_i).await?;
    let session = session.play(moonfire_rtsp::client::PlayQuirks::new()).await?.demuxed()?;

    tokio::pin!(session);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            item = session.next() => {
                match item.ok_or_else(|| format_err!("EOF"))?? {
                    CodecItem::MessageFrame(m) => {
                        info!("{}: {}\n", &m.timestamp, std::str::from_utf8(&m.data[..]).unwrap());
                    },
                    _ => continue,
                };
            },
            _ = &mut stop => {
                break;
            },
        }
    }
    Ok(())
}
