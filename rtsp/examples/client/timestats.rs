use failure::{Error, format_err};
use futures::StreamExt;
use log::info;
use moonfire_rtsp::codec::{CodecItem, Parameters};
use rtsp_types::Url;
use std::convert::TryFrom;

#[derive(structopt::StructOpt)]
pub(crate) struct Opts {
    #[structopt(default_value, long)]
    initial_timestamp_mode: moonfire_rtsp::client::InitialTimestampMode,

    #[structopt(long)]
    streams: Option<Vec<usize>>,
}

#[derive(Clone)]
struct StreamStats {
    pkts: u64,
    tot_duration: i64,
    prev_duration: u32,
    first: moonfire_rtsp::Timestamp,
    first_instant: std::time::Instant,
    latest: moonfire_rtsp::Timestamp,
    latest_instant: std::time::Instant,
}

fn process(stream_id: usize, all_stats: &mut [Option<StreamStats>], ts: moonfire_rtsp::Timestamp,
           when: std::time::Instant, duration: u32) {
    let stats = &mut all_stats[stream_id];
    let stats = match stats {
        None => {
            *stats = Some(StreamStats {
                pkts: 1,
                tot_duration: i64::from(duration),
                first: ts,
                first_instant: when,
                latest: ts,
                latest_instant: when,
                prev_duration: duration,
            });
            return;
        },
        Some(s) => s,
    };
    let tot_elapsed = i64::try_from(ts.timestamp() - stats.first.timestamp()).unwrap();
    if stats.pkts > 1 {
        let local_elapsed = (stats.latest_instant - stats.first_instant).as_secs_f64();
        let rtp_elapsed = (ts.timestamp() - stats.first.timestamp()) as f64
            / ts.clock_rate().get() as f64;
        if stats.tot_duration > 0 {
            let dur_elapsed = stats.tot_duration as f64 / ts.clock_rate().get() as f64;
            info!("stream {}: delta {:6}, rtp-local={:6.3}s rtp-dur={:6.3}s",
                  stream_id,
                  ts.timestamp() - stats.latest.timestamp(),
                  rtp_elapsed-local_elapsed,
                  rtp_elapsed-dur_elapsed);
        } else {
            info!("stream {}: delta {:6}, rtp-local={:6.3}s avg delta {:6.1}",
                  stream_id,
                  ts.timestamp() - stats.latest.timestamp(),
                  rtp_elapsed-local_elapsed,
                  (tot_elapsed as f64) / (stats.pkts as f64));
        }
    }
    stats.tot_duration += i64::from(duration);
    stats.prev_duration = duration;
    stats.latest = ts;
    stats.latest_instant = when;
    stats.pkts += 1;
}

pub(crate) async fn run(
    url: Url, credentials: Option<moonfire_rtsp::client::Credentials>, opts: Opts
) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c();

    let mut session = moonfire_rtsp::client::Session::describe(url, credentials).await?;
    info!("Streams: {:#?}", session.streams());
    let mut all_stats = vec![None; session.streams().len()];
    let mut duration_from_fps = vec![0; session.streams().len()];
    for i in 0..session.streams().len() {
        if matches!(opts.streams, Some(ref s) if s.iter().position(|si| *si == i).is_none()) {
            continue;
        }
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
    let session = session.play(
        moonfire_rtsp::client::PlayQuirks::new()
        .initial_timestamp_mode(opts.initial_timestamp_mode)
    ).await?.demuxed()?;

    tokio::pin!(session);
    tokio::pin!(stop);

    // Special-case the first GOP because the camera might buffer it for quick catch-up.
    let mut idr_count = 0;
    let mut first_idr = None;
    loop {
        tokio::select! {
            item = session.next() => {
                match item.ok_or_else(|| format_err!("EOF"))?? {
                    CodecItem::AudioFrame(f) => {
                        if idr_count < 2 {
                            continue;
                        }
                        process(
                            f.stream_id,
                            &mut all_stats,
                            f.timestamp,
                            f.ctx.msg_received(),
                            f.frame_length.get(),
                        );
                    },
                    CodecItem::VideoFrame(f) => {
                        if idr_count < 2 && !f.is_random_access_point {
                            continue;
                        } else if idr_count < 2 {
                            idr_count += 1;
                            match idr_count {
                                1 => first_idr = Some((f.ctx.msg_received(), f.timestamp)),
                                2 => {
                                    let (first_local, first_rtp) = first_idr.unwrap();
                                    println!("first GOP, rtp delta {:.3} sec in {:.3} sec",
                                             f.timestamp.elapsed_secs() - first_rtp.elapsed_secs(),
                                             (f.ctx.msg_received() - first_local).as_secs_f64());
                                },
                                _ => unreachable!(),
                            };
                        }
                        process(
                            f.stream_id,
                            &mut all_stats,
                            f.timestamp,
                            f.ctx.msg_received(),
                            duration_from_fps[f.stream_id],
                        )
                    },
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
