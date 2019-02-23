extern crate moonfire_ffmpeg;

use std::env;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

macro_rules! c_str {
    ($s:expr) => { {
        unsafe { CStr::from_ptr(concat!($s, "\0").as_ptr() as *const c_char) }
    } }
}

fn main() {
    let url = env::args().nth(1).expect("missing url");
    let _ffmpeg = moonfire_ffmpeg::Ffmpeg::new();
    let mut open_options = moonfire_ffmpeg::Dictionary::new();
    let mut input = moonfire_ffmpeg::InputFormatContext::open(&CString::new(url).unwrap(),
                                                              &mut open_options).unwrap();
    println!("open");
    input.find_stream_info().unwrap();
    let is = input.streams().get(0);
    let ic = is.codec();
    println!("pixel format: {}", ic.pix_fmt());
    let mut dopt = moonfire_ffmpeg::Dictionary::new();
    //dopt.set(c_str!("refcounted_frames"), c_str!("0")).unwrap();  // TODO?
    let d = ic.new_decoder(&mut dopt).unwrap();

    // TODO: format_name.
    let outfilename = CString::new("out.mp4").unwrap();
    let mut octx = moonfire_ffmpeg::OutputFormatContext::new(
        None, &outfilename).unwrap();
    let e = ic.codec_id().find_encoder().unwrap();
    //let mut ectx = e.alloc_context().unwrap();
    {
        let mut os = octx.add_stream(e).unwrap();
        os.codec().set_params(&ic.params());
        let mut eopt = moonfire_ffmpeg::Dictionary::new();
        os.codec().open(e, &mut eopt).unwrap();
        // TODO: avcodec_parameters_from_context equivalent?
        // TODO: global header?
    }
    octx.open(&outfilename).unwrap();
    octx.write_header().unwrap();

    let mut f = moonfire_ffmpeg::Frame::new().unwrap();
    loop {
        let pkt = match input.read_frame() {
            Ok(p) => p,
            Err(e) if e.is_eof() => { break; },
            Err(e) => panic!(e),
        };
        //println!("packet");
        if !d.decode_video(&pkt, &mut f).unwrap() {
            continue;
        }
        //println!("frame");

    }
}
