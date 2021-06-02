//! Throwaway to gather raw timestamp data for several RTSP connections into a
//! SQLite3 database. The goal is to have some data to quickly test out ideas
//! for turning real (buggy) cameras' timestamps into something useful.

use failure::Error;
use failure::ResultExt;
use failure::format_err;
use futures::Future;
use futures::FutureExt;
use futures::StreamExt;
use log::info;
use moonfire_rtsp::codec::{CodecItem, Parameters};
use parking_lot::Mutex;
use rusqlite::{named_params, params};
use rtsp_types::Url;
use std::convert::TryFrom;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::SystemTime;

#[derive(structopt::StructOpt)]
pub(crate) struct Opts {
    #[structopt(default_value, long)]
    initial_timestamp: moonfire_rtsp::client::InitialTimestampPolicy,

    #[structopt(long, parse(from_os_str))]
    db: PathBuf,

    #[structopt(parse(try_from_str))]
    urls: Vec<Url>,

    #[structopt(long, requires="password")]
    username: Option<String>,

    #[structopt(long, requires="username")]
    password: Option<String>,
}

struct StreamArgs {
    url: Url,
    creds: Option<moonfire_rtsp::client::Credentials>,
    initial_timestamp: moonfire_rtsp::client::InitialTimestampPolicy,
    stop: Pin<Box<dyn Future<Output = ()> + Send>>,
    db: Arc<Mutex<rusqlite::Connection>>,
    item_writer: tokio::sync::mpsc::UnboundedSender<Item>,
}

#[derive(Debug)]
struct Frame {
    conn_id: i64,
    stream_id: usize,
    frame_seq: u64,
    rtp_timestamp: i64,
    received_start: u64,
    received_end: u64,
    pos: u64,
    loss: u16,
    duration: Option<NonZeroU32>,
    cum_duration: Option<u64>,
    idr: Option<bool>,
}

#[derive(Debug)]
struct SenderReport {
    conn_id: i64,
    stream_id: usize,
    sr_seq: u64,
    rtp_timestamp: i64,
    received: u64,
    ntp_timestamp: moonfire_rtsp::NtpTimestamp,
}

#[derive(Debug)]
enum Item {
    Frame(Frame),
    SenderReport(SenderReport),
}

async fn stream(mut args: StreamArgs) -> Result<(), Error> {
    loop {
        let conn_id = {
            let db = args.db.lock();
            let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            db.execute(
                "insert into conn (url, start) values (?, ?)",
                params![args.url.as_str(), u64::try_from(now.as_micros())?],
            )?;
            db.last_insert_rowid()
        };
        info!("{}: conn id {} starting", &args.url, conn_id);
        let (lost_reason, reconnect) = match stream_once(conn_id, &mut args).await {
            Err(e) => (e.to_string(), true),
            Ok(false) => ("stop signal".to_owned(), false),
            Ok(true) => ("EOF".to_owned(), true),
        };
        info!("{}: conn id {} disconnected due to {}", &args.url, conn_id, &lost_reason);
        {
            let db = args.db.lock();
            let now = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap();
            db.execute(
                "update conn set lost = ?, lost_reason = ? where id = ?",
                params![u64::try_from(now.as_micros())?, &lost_reason, conn_id],
            )?;
        }
        if !reconnect {
            break;
        }
        tokio::time::sleep(std::time::Duration::new(1, 0)).await;
    }
    Ok(())
}

#[derive(Copy, Clone)]
struct StreamInfo {
    duration_from_fps: Option<NonZeroU32>,
    cum_duration: Option<u64>,
    frame_seq: u64,
    sr_seq: u64,
}

async fn stream_once(conn_id: i64, args: &mut StreamArgs) -> Result<bool, Error> {
    let mut session = moonfire_rtsp::client::Session::describe(args.url.clone(), args.creds.clone()).await?;
    let mut streams = vec![
        StreamInfo { duration_from_fps: None, cum_duration: Some(0), frame_seq: 0, sr_seq: 0 };
        session.streams().len()];
    {
        let mut db = args.db.lock();
        let tx = db.transaction()?;
        let mut insert = tx.prepare_cached(r#"
            insert into stream (conn_id, stream_id, clock_rate, media, encoding_name)
                        values (:conn_id, :stream_id, :clock_rate, :media, :encoding_name)
        "#)?;
        for (i, s) in session.streams().iter().enumerate() {
            let clock_rate = s.clock_rate;
            insert.execute(named_params! {
                ":conn_id": conn_id,
                ":stream_id": i,
                ":clock_rate": clock_rate,
                ":media": s.media,
                ":encoding_name": s.encoding_name,
            })?;
            if let Some(Parameters::Video(v)) = s.parameters() {
                if let Some((num, denom)) = v.frame_rate() {
                    if clock_rate % denom == 0 {
                        streams[i].duration_from_fps = NonZeroU32::new(num * (clock_rate / denom));
                    }
                }
            }
        }
        drop(insert);
        tx.commit()?;
    }
    for i in 0..session.streams().len() {
        session.setup(i).await?;
    }
    let session = session.play(
        moonfire_rtsp::client::PlayPolicy::default()
        .initial_timestamp(args.initial_timestamp)
        .ignore_zero_seq(true)
    ).await?.demuxed()?;

    tokio::pin!(session);

    loop {
        tokio::select! {
            item = session.next() => {
                match item.transpose()? {
                    Some(CodecItem::AudioFrame(f)) => {
                        let stream = &mut streams[f.stream_id];
                        args.item_writer.send(Item::Frame(Frame {
                            conn_id,
                            stream_id: f.stream_id,
                            frame_seq: stream.frame_seq,
                            rtp_timestamp: f.timestamp.elapsed(),
                            received_start: rescale_received(&f.ctx, f.timestamp.clock_rate()),
                            received_end: rescale_received(&f.ctx, f.timestamp.clock_rate()),
                            pos: f.ctx.msg_pos(),
                            loss: f.loss,
                            duration: Some(f.frame_length),
                            cum_duration: stream.cum_duration,
                            idr: None,
                        })).unwrap();
                        stream.frame_seq += 1;
                        stream.cum_duration =
                            Some(stream.cum_duration.unwrap() + u64::from(f.frame_length.get()));
                    },
                    Some(CodecItem::VideoFrame(f)) => {
                        let stream = &mut streams[f.stream_id];
                        args.item_writer.send(Item::Frame(Frame {
                            conn_id,
                            stream_id: f.stream_id,
                            frame_seq: stream.frame_seq,
                            rtp_timestamp: f.timestamp.elapsed(),
                            received_start: rescale_received(&f.start_ctx(), f.timestamp.clock_rate()),
                            received_end: rescale_received(&f.end_ctx(), f.timestamp.clock_rate()),
                            pos: f.start_ctx().msg_pos(),
                            loss: f.loss,
                            duration: stream.duration_from_fps,
                            cum_duration: stream.cum_duration,
                            idr: Some(f.is_random_access_point),
                        })).unwrap();
                        stream.frame_seq += 1;
                        stream.cum_duration =
                            stream.duration_from_fps.map(|d| u64::from(d.get()) * stream.frame_seq);
                    },
                    Some(CodecItem::MessageFrame(f)) => {
                        let stream = &mut streams[f.stream_id];
                        args.item_writer.send(Item::Frame(Frame {
                            conn_id,
                            stream_id: f.stream_id,
                            frame_seq: stream.frame_seq,
                            rtp_timestamp: f.timestamp.elapsed(),
                            received_start: rescale_received(&f.ctx, f.timestamp.clock_rate()),
                            received_end: rescale_received(&f.ctx, f.timestamp.clock_rate()),
                            pos: f.ctx.msg_pos(),
                            loss: f.loss,
                            duration: None,
                            cum_duration: None,
                            idr: None,
                        })).unwrap();
                        stream.frame_seq += 1;
                    },
                    Some(CodecItem::SenderReport(sr)) => {
                        args.item_writer.send(Item::SenderReport(SenderReport {
                            conn_id,
                            stream_id: sr.stream_id,
                            sr_seq: streams[sr.stream_id].sr_seq,
                            rtp_timestamp: sr.timestamp.elapsed(),
                            received: rescale_received(&sr.rtsp_ctx, sr.timestamp.clock_rate()),
                            ntp_timestamp: sr.ntp_timestamp,
                        }))?;
                        streams[sr.stream_id].sr_seq += 1;
                    },
                    None => break,
                }
            },
            _ = &mut args.stop => {
                return Ok(false);
            },
        }
    }
    Ok(true)
}

fn rescale_received(ctx: &moonfire_rtsp::Context, clock_rate: NonZeroU32) -> u64 {
    u64::try_from(
        (ctx.msg_received() - ctx.conn_established()).as_nanos()
        * u128::from(clock_rate.get())
        / 1_000_000_000).unwrap()
}

fn flush(db: &mut rusqlite::Connection, items: &mut Vec<Item>) -> Result<(), Error> {
    info!("flush of {} items", items.len());
    let tx = db.transaction()?;
    let mut insert_frame = tx.prepare_cached(r#"
        insert into frame (conn_id, stream_id, frame_seq, rtp_timestamp, received_start,
                           received_end, pos, loss, duration, cum_duration, idr)
                   values (:conn_id, :stream_id, :frame_seq, :rtp_timestamp, :received_start,
                           :received_end, :pos, :loss, :duration, :cum_duration, :idr)
    "#)?;
    let mut insert_sr = tx.prepare_cached(r#"
        insert into sender_report (conn_id, stream_id, sr_seq, rtp_timestamp, received,
                                   ntp_timestamp)
                           values (:conn_id, :stream_id, :sr_seq, :rtp_timestamp, :received,
                                   :ntp_timestamp)
    "#)?;
    for item in items.iter_mut() {
        match item {
            Item::Frame(f) => insert_frame.execute(named_params! {
                ":conn_id": &f.conn_id,
                ":stream_id": &f.stream_id,
                ":frame_seq": &f.frame_seq,
                ":rtp_timestamp": &f.rtp_timestamp,
                ":received_start": &f.received_start,
                ":received_end": &f.received_end,
                ":pos": &f.pos,
                ":loss": &f.loss,
                ":duration": &f.duration.map(NonZeroU32::get),
                ":cum_duration": &f.cum_duration,
                ":idr": &f.idr,
            }).with_context(|_| format_err!("unable to write {:#?}", &f))?,
            Item::SenderReport(sr) => insert_sr.execute(named_params! {
                ":conn_id": &sr.conn_id,
                ":stream_id": &sr.stream_id,
                ":sr_seq": &sr.sr_seq,
                ":rtp_timestamp": &sr.rtp_timestamp,
                ":received": &sr.received,
                ":ntp_timestamp": &sr.ntp_timestamp.0.wrapping_sub(moonfire_rtsp::UNIX_EPOCH.0),
            }).with_context(|_| format_err!("unable to write {:#?}", &sr))?,
        };
    }
    drop(insert_frame);
    drop(insert_sr);
    tx.commit()?;
    items.clear();
    Ok(())
}

pub(crate) async fn run(mut opts: Opts) -> Result<(), Error> {
    let stop = tokio::signal::ctrl_c()
        .map(|r: std::io::Result<()> | {
            if let Err(e) = r {
                log::error!("ctrl_c future failed: {:#?}", e);
            }
            ()
        })
        .shared();

    let mut db = rusqlite::Connection::open(&opts.db)?;
    db.pragma_update(None, "journal_mode", &"wal")?;
    let tx = db.transaction()?;
    tx.execute_batch(include_str!("timedump.sql"))?;
    tx.commit()?;
    info!("db created");

    let db = Arc::new(Mutex::new(db));

    let creds = super::creds(opts.username, opts.password);
    let (item_writer, mut item_reader) = tokio::sync::mpsc::unbounded_channel();

    let initial_timestamp = opts.initial_timestamp;
    info!("spawning workers");
    let joins: Vec<_> = opts.urls.drain(..)
        .map(|url| tokio::spawn(stream(StreamArgs {
            url,
            creds: creds.clone(),
            stop: stop.clone().boxed(),
            initial_timestamp,
            db: db.clone(),
            item_writer: item_writer.clone(),

        }))).collect();
    drop(item_writer);

    const BUF_SIZE: usize = 1024;
    let mut items = Vec::with_capacity(BUF_SIZE);
    while let Some(item) = item_reader.recv().await {
        items.push(item);
        if items.len() >= BUF_SIZE {
            flush(&mut db.lock(), &mut items)?;
        }
    }
    flush(&mut db.lock(), &mut items)?;

    for j in joins {
        j.await.unwrap().unwrap();
    }
    Ok(())
}
