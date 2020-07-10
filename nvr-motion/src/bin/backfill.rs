//! Runs video analytics over the entire corpus.
//! Currently doesn't actually do anything with them; just getting the workflow down.
//! TODO: keep state.

use cstr::*;
use failure::{Error, bail, format_err};
use log::{info, trace};
use moonfire_ffmpeg::avutil::VideoFrame;
use rayon::prelude::*;
use rusqlite::params;
use std::convert::TryFrom;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
use structopt::StructOpt;
use uuid::Uuid;

#[derive(StructOpt)]
struct Opt {
    #[structopt(short, long, parse(try_from_str))]
    cookie: Option<reqwest::header::HeaderValue>,

    #[structopt(short, long, parse(try_from_str))]
    nvr: reqwest::Url,

    #[structopt(short, long, parse(from_os_str))]
    db: std::path::PathBuf,

    #[structopt(short, long, parse(try_from_str))]
    start: Option<moonfire_nvr_client::Time>,

    #[structopt(short, long, parse(try_from_str))]
    end: Option<moonfire_nvr_client::Time>,

    #[structopt(short="f", long)]
    fps: Option<f32>,

    #[structopt(short="C", long, use_delimiter=true)]
    cameras: Option<Vec<String>>,
}

struct Context<'a> {
    conn: parking_lot::Mutex<rusqlite::Connection>,

    // Stuff for fetching recordings.
    client: moonfire_nvr_client::Client,
    start: Option<moonfire_nvr_client::Time>,
    end: Option<moonfire_nvr_client::Time>,
    cameras: Option<Vec<String>>,

    // Stuff for processing recordings.
    // This supports using multiple interpreters, one per Edge TPU device.
    // Use a crossbeam channel as a crude object pool: receive one, use it, send it back.
    interpreter_tx: crossbeam::channel::Sender<moonfire_tflite::Interpreter<'a>>,
    interpreter_rx: crossbeam::channel::Receiver<moonfire_tflite::Interpreter<'a>>,
    width: usize,
    height: usize,
    min_interval_90k: i32,
    frames_processed: AtomicUsize,
}

/// Gets the id range of committed recordings indicated by `r`.
fn id_range(r: &moonfire_nvr_client::Recording) -> std::ops::Range<i32> {
    let end_id = r.first_uncommitted.unwrap_or(r.end_id.unwrap_or(r.start_id) + 1);
    r.start_id .. end_id
}

/// Removes values from sorted Vec `from` if they are also in sorted Iterator `remove`.
/// Takes O(from.len() + remove.len()) time.
fn filter_sorted<'a, T: 'a + Ord, I: Iterator<Item = &'a T>>(from: &mut Vec<T>, mut remove: I) {
    let mut cur_remove = remove.next();
    from.retain(|e| {
        while let Some(r) = cur_remove.as_ref() {
            match e.cmp(r) {
                std::cmp::Ordering::Less => return true,
                std::cmp::Ordering::Equal => return false,
                std::cmp::Ordering::Greater => cur_remove = remove.next(),
            }
        }
        true
    });
}

async fn list_recordings(ctx: &Context<'_>) -> Result<Vec<Option<(Stream, Vec<i32>)>>, Error> {
    let top_level = ctx.client.top_level(&moonfire_nvr_client::TopLevelRequest::default())
        .await?;
    Ok(futures::future::try_join_all(
        top_level.cameras.iter().map(|c| process_camera(&ctx, c))).await?)
}

async fn process_camera(ctx: &Context<'_>, camera: &moonfire_nvr_client::Camera)
                        -> Result<Option<(Stream, Vec<i32>)>, Error> {
    const DESIRED_STREAM: &str = "sub";
    if let Some(cameras) = &ctx.cameras {
        if !cameras.contains(&camera.short_name) {
            return Ok(None);
        }
    }
    if !camera.streams.contains_key(DESIRED_STREAM) {
        return Ok(None);
    }
    let recordings = ctx.client.list_recordings(&moonfire_nvr_client::ListRecordingsRequest {
        camera: camera.uuid,
        stream: DESIRED_STREAM,
        start: ctx.start,
        end: ctx.end,
    }).await?;

    let num_recordings = recordings.recordings.iter().map(|r| {
        let range = id_range(r);
        usize::try_from(range.end - range.start).unwrap()
    }).sum();
    let mut ids = Vec::with_capacity(num_recordings);
    for r in &recordings.recordings {
        for id in id_range(r) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return Ok(None);
    }
    ids.sort();  // it's probably sorted, but make sure.
    let conn = ctx.conn.lock();
    let mut stmt = conn.prepare_cached(r#"
        select
          recording_id
        from
          recording_object_detection
        where
          camera_uuid = ? and
          stream_name = ? and
          ? <= recording_id and
          recording_id <= ?
        order by recording_id
    "#)?;
    let u = camera.uuid.as_bytes();
    let existing = stmt
        .query_map(params![&u[..], DESIRED_STREAM, ids.first().unwrap(), ids.last().unwrap()],
                   |row| row.get::<_, i32>(0))?
        .collect::<Result<Vec<i32>, rusqlite::Error>>()?;
    filter_sorted(&mut ids, existing.iter());
    let stream = Stream {
        camera_short_name: camera.short_name.clone(),
        camera_uuid: camera.uuid,
        stream_name: DESIRED_STREAM.to_owned(),
    };
    Ok(Some((stream, ids)))
}

struct Stream {
    camera_short_name: String,
    camera_uuid: Uuid,
    stream_name: String,
}

struct Recording {
    stream_i: u32,
    id: i32,
    body: bytes::Bytes,
}

async fn fetch_recording(ctx: &Context<'_>, stream: &Stream, id: i32)
                         -> Result<bytes::Bytes, Error> {
    trace!("recording {}/{}/{}", &stream.camera_short_name, &stream.stream_name, id);
    let resp = ctx.client.view(&moonfire_nvr_client::ViewRequest {
        camera: stream.camera_uuid,
        mp4_type: moonfire_nvr_client::Mp4Type::Normal,
        stream: &stream.stream_name,
        s: &id.to_string(),
        ts: false,
    }).await?;
    Ok(resp.bytes().await?)
}

pub fn zigzag32(i: i32) -> u32 { ((i << 1) as u32) ^ ((i >> 31) as u32) }

pub fn append_varint32(i: u32, data: &mut Vec<u8>) {
    if i < 1u32 << 7 {
        data.push(i as u8);
    } else if i < 1u32 << 14 {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                   (i >>  7)                 as u8]);
    } else if i < 1u32 << 21 {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                 (((i >>  7) & 0x7F) | 0x80) as u8,
                                   (i >> 14)                 as u8]);
    } else if i < 1u32 << 28 {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                 (((i >>  7) & 0x7F) | 0x80) as u8,
                                 (((i >> 14) & 0x7F) | 0x80) as u8,
                                   (i >> 21)                 as u8]);
    } else {
        data.extend_from_slice(&[(( i        & 0x7F) | 0x80) as u8,
                                 (((i >>  7) & 0x7F) | 0x80) as u8,
                                 (((i >> 14) & 0x7F) | 0x80) as u8,
                                 (((i >> 21) & 0x7F) | 0x80) as u8,
                                   (i >> 28)                 as u8]);
    }
}

const SCORE_THRESHOLD: f32 = 0.5;

fn normalize(v: f32) -> u8 {
    (v.max(0.).min(1.0) * 255.) as u8
}

pub fn append_frame(interpreter: &moonfire_tflite::Interpreter<'_>, data: &mut Vec<u8>) {
    let outputs = interpreter.outputs();
    let boxes = outputs[0].f32s();
    let classes = outputs[1].f32s();
    let scores = outputs[2].f32s();
    let num_labels = scores.iter().filter(|&&s| s >= SCORE_THRESHOLD).count();
    append_varint32(u32::try_from(num_labels).unwrap(), data);
    for (i, &score) in scores.iter().enumerate() {
        if score < SCORE_THRESHOLD {
            continue;
        }
        let box_ = &boxes[4*i..4*i+4];
        let y = normalize(box_[0]);
        let x = normalize(box_[1]);
        let h = normalize(box_[2] - box_[0]);
        let w = normalize(box_[3] - box_[1]);
        append_varint32(classes[i] as u32, data);
        data.push(x);
        data.push(w);
        data.push(y);
        data.push(h);
        data.push(normalize(scores[i]));
    }
}

fn process_recording(ctx: &Context<'_>, streams: &Vec<&Stream>, recording: &Recording)
                     -> Result<(), Error> {
    let mut open_options = moonfire_ffmpeg::avutil::Dictionary::new();
    let mut io_ctx = moonfire_ffmpeg::avformat::SliceIoContext::new(&recording.body);
    let mut input = moonfire_ffmpeg::avformat::InputFormatContext::with_io_context(
        cstr!(""), &mut io_ctx, &mut open_options).unwrap();
    input.find_stream_info().unwrap();

    // In .mp4 files generated by Moonfire NVR, the video is always stream 0.
    // The timestamp subtitles (if any) are stream 1.
    const VIDEO_STREAM: usize = 0;

    let stream = input.streams().get(VIDEO_STREAM);
    let par = stream.codecpar();
    let mut dopt = moonfire_ffmpeg::avutil::Dictionary::new();
    dopt.set(cstr!("refcounted_frames"), cstr!("0")).unwrap();  // TODO?
    let d = par.new_decoder(&mut dopt).unwrap();

    let mut scaled = VideoFrame::owned(moonfire_ffmpeg::avutil::ImageDimensions {
        width: i32::try_from(ctx.width).unwrap(),
        height: i32::try_from(ctx.height).unwrap(),
        pix_fmt: moonfire_ffmpeg::avutil::PixelFormat::rgb24(),
    }).unwrap();
    let mut f = VideoFrame::empty().unwrap();
    let mut s = moonfire_ffmpeg::swscale::Scaler::new(par.dims(), scaled.dims()).unwrap();

    let mut frame_data = Vec::with_capacity(4096);
    let mut durations = Vec::with_capacity(4096);
    let mut last_duration = 0;
    let mut next_pts = 0;
    append_varint32(u32::try_from(ctx.min_interval_90k).unwrap(), &mut frame_data);
    loop {
        let pkt = match input.read_frame() {
            Ok(p) => p,
            Err(e) if e.is_eof() => { break; },
            Err(e) => panic!(e),
        };
        if pkt.stream_index() != VIDEO_STREAM {
            continue;
        }
        if !d.decode_video(&pkt, &mut f).unwrap() {
            continue;
        }
        let d = pkt.duration();
        append_varint32(zigzag32(d.checked_sub(last_duration).unwrap()), &mut durations);
        last_duration = d;
        let pts = pkt.pts().unwrap();
        match pts.cmp(&next_pts) {
            std::cmp::Ordering::Less => continue,
            std::cmp::Ordering::Equal => {},
            std::cmp::Ordering::Greater => {
                // works for non-negative values.
                fn ceil_div(a: i64, b: i64) -> i64 { (a + b - 1) / b }
                let i = i64::from(ctx.min_interval_90k);
                let before = next_pts;
                next_pts = ceil_div(pts, i) * i;
                assert!(next_pts >= pts, "next_pts {}->{} pts {} interval {}",
                        before, next_pts, pts, i);
            },
        }

        // Perform object detection on the frame.
        s.scale(&f, &mut scaled);
        let mut interpreter = ctx.interpreter_rx.recv().unwrap();
        moonfire_motion::copy(&scaled, &mut interpreter.inputs()[0]);
        interpreter.invoke().unwrap();
        ctx.frames_processed.fetch_add(1, Ordering::Relaxed);
        append_frame(&interpreter, &mut frame_data);
        ctx.interpreter_tx.try_send(interpreter).unwrap();
    }
    let compressed = zstd::stream::encode_all(&frame_data[..], 22)?;

    let conn = ctx.conn.lock();
    let mut stmt = conn.prepare_cached(r#"
        insert into recording_object_detection (camera_uuid, stream_name, recording_id, frame_data,
                                                durations)
            values (?, ?, ?, ?, ?)
    "#)?;
    let stream = streams[usize::try_from(recording.stream_i).unwrap()];
    let u = stream.camera_uuid.as_bytes();
    stmt.execute(params![&u[..], &stream.stream_name, &recording.id, &compressed, &durations])?;
    Ok(())
}

fn main() -> Result<(), Error> {
    let mut h = moonfire_motion::init_logging();
    let _a = h.async_scope();
    let opt = Opt::from_args();

    let conn = parking_lot::Mutex::new(rusqlite::Connection::open(&opt.db)?);

    info!("Loading model");
    let m = moonfire_tflite::Model::from_static(moonfire_motion::MODEL).unwrap();
    info!("Creating interpreters");
    let devices = moonfire_tflite::edgetpu::Devices::list();
    if devices.is_empty() {
        bail!("no edge tpu ready");
    }
    let delegates = devices
        .into_iter()
        .map(|d| d.create_delegate())
        .collect::<Result<Vec<_>, ()>>()
        .map_err(|()| format_err!("Unable to create delegate"))?;
    let mut interpreters = delegates.iter().map(|d| {
        let mut builder = moonfire_tflite::Interpreter::builder();
        builder.add_borrowed_delegate(d);
        builder.build(&m)
    }).collect::<Result<Vec<_>, ()>>().map_err(|()| format_err!("Unable to build interpreter"))?;
    info!("Done creating {} interpreters", interpreters.len());

    let (width, height);
    {
        let inputs = interpreters[0].inputs();
        let input = &inputs[0];
        let num_dims = input.num_dims();
        assert_eq!(num_dims, 4);
        assert_eq!(input.dim(0), 1);
        height = input.dim(1);
        width = input.dim(2);
        assert_eq!(input.dim(3), 3);
    }

    // Fill the interpreter "pool" (channel).
    let (interpreter_tx, interpreter_rx) = crossbeam::channel::bounded(interpreters.len());
    for i in interpreters.into_iter() {
        interpreter_tx.try_send(i).unwrap();
    }

    let min_interval_90k = match opt.fps {
        None => 1,
        Some(f) if f > 0. => (90000. / f) as i32,
        Some(_) => panic!("interval fps; must be non-negative"),
    };
    assert!(min_interval_90k > 0);

    let ctx = Context {
        client: moonfire_nvr_client::Client::new(opt.nvr, opt.cookie),
        conn,
        interpreter_tx,
        interpreter_rx,
        width,
        height,
        start: opt.start,
        end: opt.end,
        cameras: opt.cameras,
        min_interval_90k,
        frames_processed: AtomicUsize::new(0),
    };

    let _ffmpeg = moonfire_ffmpeg::Ffmpeg::new();
    let mut rt = tokio::runtime::Runtime::new()?;

    info!("Finding recordings");
    let stuff = rt.block_on(list_recordings(&ctx))?;
    let mut streams = Vec::new();
    let mut count = 0;
    for s in &stuff {
        if let Some((stream, ids)) = s.as_ref() {
            streams.push(stream);
            count += u64::try_from(ids.len()).unwrap();
        }
    }

    info!("Found {} recordings", count);
    let progress = Arc::new(indicatif::ProgressBar::new(count)
        .with_style(indicatif::ProgressStyle::default_bar()
            .template("[{eta_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
            .progress_chars("##-")));
    progress.enable_steady_tick(100);

    let (decode_tx, decode_rx) = crossbeam::channel::bounded(16);
    let mut decode_tx = Some(decode_tx);

    let start = std::time::Instant::now();
    rayon::scope(|s| {
        // Decoder threads.
        s.spawn(|_| {
            let before = std::time::Instant::now();
            info!("Decoder thread starting");
            decode_rx.iter().par_bridge().try_for_each(|r: Recording| -> Result<(), Error> {
                process_recording(&ctx, &streams, &r)?;
                let frames_processed = ctx.frames_processed.load(Ordering::Relaxed);
                let elapsed = std::time::Instant::now() - start;
                info!("rate = {:.1} fps", frames_processed as f32 / elapsed.as_secs_f32());
                progress.inc(1);
                Ok(())
            }).unwrap();
            info!("Decoder thread ending after {:?}", before.elapsed());
            info!("asdf");
        });

        // Fetch thread.
        // TODO: fetch thread per sample file dir? or maybe unnecessary, fast enough as is.
        s.spawn(|_| {
            let mut stream_i = 0;
            let mut fetch_time = std::time::Duration::new(0, 0);
            let mut send_time = std::time::Duration::new(0, 0);
            let decode_tx = decode_tx.take().unwrap();
            for s in &stuff {
                if let Some((stream, ids)) = s {
                    for &id in ids {
                        let before = std::time::Instant::now();
                        let body = rt.block_on(fetch_recording(&ctx, &stream, id)).unwrap();
                        let between = std::time::Instant::now();
                        decode_tx.send(Recording {
                            stream_i,
                            id,
                            body,
                        }).unwrap();
                        let after = std::time::Instant::now();
                        fetch_time += between.checked_duration_since(before).unwrap();
                        send_time += after.elapsed();
                    }
                    stream_i += 1;
                }
            }
            info!("Fetch finishing; fetch time={:?} send time={:?}", fetch_time, send_time);
        });
    });

    progress.finish();
    Ok(())
}

#[cfg(test)]
mod test {
    #[test]
    fn filter() {
        let mut from = vec![1, 3, 5, 10, 20];
        super::filter_sorted(&mut from, [1, 2, 3, 8, 20].iter());
        assert_eq!(&from, &[5, 10]);
    }
}
