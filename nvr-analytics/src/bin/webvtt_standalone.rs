/// Writes a WebVTT metadata caption file representing all of the objects detected in the given
/// .mp4 file. Each cue represents a single object for a single frame.

use cstr::*;
use moonfire_ffmpeg::avutil::{Rational, VideoFrame};
use serde::Serialize;
use std::convert::TryFrom;
use std::env;
use std::ffi::CString;
use std::io::Write;

#[derive(Serialize)]
struct Object {
    label: &'static str,
    score: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

struct Pts(i64, Rational);

impl std::fmt::Display for Pts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // https://www.w3.org/TR/webvtt1/#webvtt-timestamp
        let seconds = self.0 as f64 * self.1.num as f64 / self.1.den as f64;
        let minutes = (seconds / 60.).trunc();
        let seconds = seconds % 60.;
        let hours = (minutes / 60.).trunc();
        let minutes = minutes % 60.;
        write!(f, "{:02.0}:{:02.0}:{:06.3}", hours, minutes, seconds)
    }
}

fn write_objs(mut stdout: &mut dyn Write, start: Pts, end: Pts, objs: &[Object])
              -> std::io::Result<()> {
    for o in objs {
        write!(stdout, "{} --> {}\n", start, end)?;
        serde_json::to_writer(&mut stdout, o)?;
        write!(stdout, "\n\n")?;
    }
    if !objs.is_empty() {
        stdout.flush()?;
    }
    Ok(())
}

fn main() {
    let m = moonfire_tflite::Model::from_static(nvr_analytics::MODEL).unwrap();
    let delegate;
    let mut builder = moonfire_tflite::Interpreter::builder();
    let devices = moonfire_tflite::edgetpu::Devices::list();
    if !devices.is_empty() {
        delegate = devices[0].create_delegate().unwrap();
        builder.add_borrowed_delegate(&delegate);
    }
    let mut interpreter = builder.build(&m).unwrap();

    let (width, height);
    {
        let inputs = interpreter.inputs();
        let input = &inputs[0];
        let num_dims = input.num_dims();
        assert_eq!(num_dims, 4);
        assert_eq!(input.dim(0), 1);
        height = input.dim(1);
        width = input.dim(2);
        assert_eq!(input.dim(3), 3);
    }

    let url = env::args().nth(1).expect("missing url");
    let _ffmpeg = moonfire_ffmpeg::Ffmpeg::new();
    let mut open_options = moonfire_ffmpeg::avutil::Dictionary::new();
    let mut input = moonfire_ffmpeg::avformat::InputFormatContext::open(&CString::new(url).unwrap(),
                                                              &mut open_options).unwrap();
    input.find_stream_info().unwrap();

    // In .mp4 files generated by Moonfire NVR, the video is always stream 0.
    // The timestamp subtitles (if any) are stream 1.
    const VIDEO_STREAM: usize = 0;

    let stream = input.streams().get(VIDEO_STREAM);
    let time_base = stream.time_base();
    let par = stream.codecpar();
    let mut dopt = moonfire_ffmpeg::avutil::Dictionary::new();
    dopt.set(cstr!("refcounted_frames"), cstr!("0")).unwrap();  // TODO?
    let d = par.new_decoder(&mut dopt).unwrap();

    let mut scaled = VideoFrame::owned(moonfire_ffmpeg::avutil::ImageDimensions {
        width: i32::try_from(width).unwrap(),
        height: i32::try_from(height).unwrap(),
        pix_fmt: moonfire_ffmpeg::avutil::PixelFormat::rgb24(),
    }).unwrap();
    let mut f = VideoFrame::empty().unwrap();
    let mut s = moonfire_ffmpeg::swscale::Scaler::new(par.dims(), scaled.dims()).unwrap();
    let mut prev_pts = 0;
    let mut prev_objs: Vec<Object> = Vec::new();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();
    write!(&mut stdout, "WEBVTT\n\n").unwrap();
    loop {
        let pkt = match input.read_frame() {
            Ok(p) => p,
            Err(e) if e.is_eof() => { break; },
            Err(e) => panic!("{}", e),
        };
        if pkt.stream_index() != VIDEO_STREAM {
            continue;
        }
        if !d.decode_video(&pkt, &mut f).unwrap() {
            continue;
        }
        write_objs(&mut stdout, Pts(prev_pts, time_base), Pts(f.pts(), time_base),
                   &prev_objs).unwrap();
        prev_objs.clear();
        prev_pts = f.pts();
        s.scale(&f, &mut scaled);
        nvr_analytics::copy(&scaled, &mut interpreter.inputs()[0]);
        interpreter.invoke().unwrap();
        let outputs = interpreter.outputs();
        let boxes = outputs[0].f32s();
        let classes = outputs[1].f32s();
        let scores = outputs[2].f32s();
        for (i, &score) in scores.iter().enumerate() {
            if score <= 0.5 {
                continue;
            }
            let class = classes[i];
            let l = nvr_analytics::label(class);
            let box_ = &boxes[4*i..4*i+4];
            if let Some(label) = l {
                prev_objs.push(Object {
                    y: box_[0],
                    x: box_[1],
                    h: box_[2] - box_[0],
                    w: box_[3] - box_[1],
                    label,
                    score,
                });
            }
        }
    }
    write_objs(&mut stdout, Pts(prev_pts, time_base), Pts(stream.duration(), time_base),
               &prev_objs).unwrap();
}
