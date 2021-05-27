use failure::{Error, format_err};
use futures::StreamExt;
use log::info;
use moonfire_rtsp::codec::{CodecItem, Parameters};
use rtsp_types::Url;
use std::convert::TryFrom;

#[derive(Clone)]
struct StreamStats {
    pkts: u64,
    tot_duration: i64,
    first: moonfire_rtsp::Timestamp,
    latest: moonfire_rtsp::Timestamp,
}

fn process(stream_id: usize, all_stats: &mut [Option<StreamStats>], ts: moonfire_rtsp::Timestamp,
           duration: u32) {
    let stats = &mut all_stats[stream_id];
    let stats = match stats {
        None => {
            *stats = Some(StreamStats {
                pkts: 1,
                tot_duration: i64::from(duration),
                first: ts,
                latest: ts,
            });
            return;
        },
        Some(s) => s,
    };
    let tot_elapsed = i64::try_from(ts.timestamp() - stats.first.timestamp()).unwrap();
    if stats.pkts > 1 {
        let elapsed = ts.timestamp() - stats.latest.timestamp();
        if stats.tot_duration > 0 {
            info!("stream {}: delta {:6}, ahead by {:6}",
                  stream_id, elapsed, tot_elapsed - stats.tot_duration);
        } else {
            info!("stream {}: delta {:6}, avg {:6.1}",
                    stream_id, elapsed, (tot_elapsed as f64) / (stats.pkts as f64));
        }
    }
    stats.tot_duration += i64::from(duration);
    stats.latest = ts;
    stats.pkts += 1;
}

pub async fn run(url: Url, credentials: Option<moonfire_rtsp::client::Credentials>) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c();

    let mut session = moonfire_rtsp::client::Session::describe(url, credentials).await?;
    let mut all_stats = vec![None; session.streams().len()];
    let mut duration_from_fps = vec![0; session.streams().len()];
    for i in 0..session.streams().len() {
        if let Some(Parameters::Video(v)) = session.streams()[i].parameters() {
            if let Some((num, denom)) = v.frame_rate() {
                let clock_rate = session.streams()[i].clock_rate;
                if clock_rate % denom == 0 {
                    duration_from_fps[i] = num * (clock_rate / denom);
                }
            }
        }
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
                    CodecItem::AudioFrame(f) => process(
                        f.stream_id,
                        &mut all_stats,
                        f.timestamp,
                        f.frame_length.get(),
                    ),
                    CodecItem::VideoFrame(f) => process(
                        f.stream_id,
                        &mut all_stats,
                        f.timestamp,
                        duration_from_fps[f.stream_id],
                    ),
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
