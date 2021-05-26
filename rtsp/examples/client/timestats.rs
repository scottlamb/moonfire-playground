//use bytes::Buf;
use failure::{Error, format_err};
use futures::StreamExt;
use log::info;
use moonfire_rtsp::client::DemuxedItem;
use rtsp_types::Url;

#[derive(Clone)]
struct StreamStats {
    pkts: u64,
    first: moonfire_rtsp::Timestamp,
    latest: moonfire_rtsp::Timestamp,
}

fn process(stream_id: usize, all_stats: &mut [Option<StreamStats>], ts: moonfire_rtsp::Timestamp) {
    let stats = &mut all_stats[stream_id];
    let stats = match stats {
        None => {
            *stats = Some(StreamStats {
                pkts: 1,
                first: ts,
                latest: ts,
            });
            return;
        },
        Some(s) => s,
    };
    let tot_elapsed = ts.timestamp() - stats.first.timestamp();
    if stats.pkts > 1 {
        let elapsed = ts.timestamp() - stats.latest.timestamp();
        info!("stream {}: delta {:6}, avg {:6.1}",
                stream_id, elapsed, (tot_elapsed as f64) / (stats.pkts as f64));
    }
    stats.latest = ts;
    stats.pkts += 1;
}

pub async fn run(url: Url, credentials: Option<moonfire_rtsp::client::Credentials>) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c();

    let mut session = moonfire_rtsp::client::Session::describe(url, credentials).await?;
    let mut all_stats = vec![None; session.streams().len()];
    for i in 0..session.streams().len() {
        session.setup(i).await?;
    }
    let session = session.play().await?.demuxed()?;


    // Read RTP data.
    tokio::pin!(session);
    tokio::pin!(stop);

    loop {
        tokio::select! {
            item = session.next() => {
                match item.ok_or_else(|| format_err!("EOF"))?? {
                    DemuxedItem::AudioFrame(f) => process(f.stream_id, &mut all_stats, f.timestamp),
                    DemuxedItem::Picture(p) => process(p.stream_id, &mut all_stats, p.timestamp),
                    _ => {},
                };
            },
            _ = &mut stop => {
                break;
            },
        }
    }
    Ok(())
}
