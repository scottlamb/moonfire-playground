extern crate moonfire_ffmpeg;
extern crate moonfire_motion;

use moonfire_motion::{Processor, MotionProcessor};

//use cstr::*;
use std::env;
use std::ffi::CString;

fn main() {
    let url = env::args().nth(1).expect("missing url");
    let _ffmpeg = moonfire_ffmpeg::Ffmpeg::new();
    let mut open_options = moonfire_ffmpeg::Dictionary::new();
    let mut input = moonfire_ffmpeg::InputFormatContext::open(&CString::new(url).unwrap(),
                                                              &mut open_options).unwrap();
    println!("open");
    input.find_stream_info().unwrap();
    let s = input.streams().get(0);
    let cp = s.codecpar();
    let mut dopt = moonfire_ffmpeg::Dictionary::new();
    //dopt.set(cstr!("refcounted_frames"), cstr!("0")).unwrap();  // TODO?
    let d = cp.new_decoder(&mut dopt).unwrap();
    //println!("pixel format: {}", d.pix_fmt());
    //let img = moonfire_ffmpeg::Image::new(c.width(), c.height(), c.pix_fmt(), 1).unwrap();
    let mut f = moonfire_ffmpeg::Frame::new().unwrap();
    let mut p: Option<MotionProcessor> = None;
    loop {
        let pkt = match input.read_frame() {
            Ok(p) => p,
            Err(e) if e.is_eof() => { break; },
            Err(e) => panic!(e),
        };
        println!("packet");
        if !d.decode_video(&pkt, &mut f).unwrap() {
            continue;
        }
        println!("frame");
        p = Some(match p {
            None => MotionProcessor::new(&f),
            Some(mut p) => { p.process(&f).unwrap(); p },
        });
    }
}
