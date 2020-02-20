// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2017 Scott Lamb <slamb@slamb.org>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use log::info;
use parking_lot::Once;
use std::cell::{Ref, RefCell};
use std::ffi::CStr;
use std::fmt::{self, Write};
use std::ptr;

static START: Once = Once::new();

//#[link(name = "avcodec")]
extern "C" {
    fn avcodec_version() -> libc::c_int;
    fn avcodec_alloc_context3(codec: *const AVCodec) -> *mut AVCodecContext;
    fn avcodec_decode_video2(ctx: *const AVCodecContext, picture: *mut AVFrame,
                             got_picture_ptr: *mut libc::c_int,
                             pkt: *const AVPacket) -> libc::c_int;
    fn avcodec_find_decoder(codec_id: libc::c_int) -> *const AVCodec;
    fn avcodec_find_encoder(codec_id: libc::c_int) -> *const AVCodec;
    fn avcodec_free_context(ctx: *mut *mut AVCodecContext);
    fn avcodec_open2(ctx: *mut AVCodecContext, codec: *const AVCodec,
                     options: *mut *mut AVDictionary) -> libc::c_int;
    fn avcodec_parameters_to_context(ctx: *mut AVCodecContext, par: *const AVCodecParameters)
                                     -> libc::c_int;
    fn av_init_packet(p: *mut AVPacket);
    fn av_packet_unref(p: *mut AVPacket);
}

//#[link(name = "avformat")]
extern "C" {
    fn avformat_version() -> libc::c_int;

    //fn avformat_alloc_output_context2(ctx: *mut *mut AVFormatContext, oformat: *mut AVOutputFormat,
    //                                  format_name: *const libc::c_char,
    //                                  filename: *const libc::c_char) -> libc::c_int;
    fn avformat_open_input(ctx: *mut *mut AVFormatContext, url: *const libc::c_char,
                           fmt: *const AVInputFormat, options: *mut *mut AVDictionary)
                           -> libc::c_int;
    fn avformat_close_input(ctx: *mut *mut AVFormatContext);
    fn avformat_find_stream_info(ctx: *mut AVFormatContext, options: *mut *mut AVDictionary)
                                 -> libc::c_int;
    //fn avformat_new_stream(s: *mut AVFormatContext, c: *const AVCodec) -> *mut AVStream;
    //fn avformat_write_header(c: *mut AVFormatContext, opts: *mut *mut AVDictionary) -> libc::c_int;
    fn av_read_frame(ctx: *mut AVFormatContext, p: *mut AVPacket) -> libc::c_int;
    fn av_register_all();
    fn avformat_network_init() -> libc::c_int;
}

//#[link(name = "avutil")]
extern "C" {
    fn avutil_version() -> libc::c_int;
    fn av_strerror(e: libc::c_int, b: *mut libc::c_char, s: libc::size_t) -> libc::c_int;
    fn av_dict_count(d: *const AVDictionary) -> libc::c_int;
    fn av_dict_get(d: *const AVDictionary, key: *const libc::c_char, prev: *mut AVDictionaryEntry,
                   flags: libc::c_int) -> *mut AVDictionaryEntry;
    fn av_dict_set(d: *mut *mut AVDictionary, key: *const libc::c_char, value: *const libc::c_char,
                   flags: libc::c_int) -> libc::c_int;
    fn av_dict_free(d: *mut *mut AVDictionary);
    fn av_frame_alloc() -> *mut AVFrame;
    fn av_frame_free(f: *mut *mut AVFrame);
    fn av_get_pix_fmt_name(fmt: libc::c_int) -> *const libc::c_char;
}

//#[link(name = "swscale")]
extern "C" {
    fn swscale_version() -> libc::c_int;

    fn sws_getContext(src_w: libc::c_int, src_h: libc::c_int, src_fmt: libc::c_int,
                      dst_w: libc::c_int, dst_h: libc::c_int, dst_fmt: libc::c_int,
                      flags: libc::c_int, src_filter: *mut SwsFilter,
                      dst_filter: *mut SwsFilter, param: *const libc::c_double) -> *mut SwsContext;
    fn sws_freeContext(ctx: *mut SwsContext);
    fn sws_scale(c: *mut SwsContext, src_slice: *const *const u8, src_stride: *const libc::c_int,
                 src_slice_y: libc::c_int, src_slice_h: libc::c_int,
                 dst_slice: *const *mut u8, dst_stride: *const libc::c_int);
}

//#[link(name = "wrapper")]
extern "C" {
    static moonfire_ffmpeg_compiled_libavcodec_version: libc::c_int;
    static moonfire_ffmpeg_compiled_libavformat_version: libc::c_int;
    static moonfire_ffmpeg_compiled_libavutil_version: libc::c_int;
    static moonfire_ffmpeg_compiled_libswscale_version: libc::c_int;
    static moonfire_ffmpeg_av_dict_ignore_suffix: libc::c_int;
    static moonfire_ffmpeg_av_nopts_value: i64;

    static moonfire_ffmpeg_av_codec_id_h264: libc::c_int;
    static moonfire_ffmpeg_avmedia_type_video: libc::c_int;

    static moonfire_ffmpeg_averror_eof: libc::c_int;
    static moonfire_ffmpeg_averror_enomem: libc::c_int;
    static moonfire_ffmpeg_averror_decoder_not_found: libc::c_int;
    static moonfire_ffmpeg_averror_unknown: libc::c_int;

    static moonfire_ffmpeg_sws_bilinear: libc::c_int;

    fn moonfire_ffmpeg_init();

    // avcodec
    fn moonfire_ffmpeg_codecpar_codec_id(ctx: *const AVCodecParameters) -> libc::c_int;
    fn moonfire_ffmpeg_codecpar_codec_type(ctx: *const AVCodecParameters) -> libc::c_int;
    fn moonfire_ffmpeg_codecpar_dims(ctx: *const AVCodecParameters) -> ImageDimensions;
    fn moonfire_ffmpeg_codecpar_extradata(ctx: *const AVCodecParameters) -> DataLen;

    fn moonfire_ffmpeg_cctx_codec_id(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_codec_type(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_extradata(ctx: *const AVCodecContext) -> DataLen;
    fn moonfire_ffmpeg_cctx_pix_fmt(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_height(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_width(ctx: *const AVCodecContext) -> libc::c_int;
    fn moonfire_ffmpeg_cctx_params(ctx: *const AVCodecContext, p: *mut VideoParameters);
    fn moonfire_ffmpeg_cctx_set_params(ctx: *mut AVCodecContext, p: *const VideoParameters);

    fn moonfire_ffmpeg_frame_stuff(frame: *const AVFrame, stuff: *mut FrameStuff);

    fn moonfire_ffmpeg_packet_alloc() -> *mut AVPacket;
    fn moonfire_ffmpeg_packet_free(p: *mut AVPacket);
    fn moonfire_ffmpeg_packet_is_key(p: *const AVPacket) -> bool;
    fn moonfire_ffmpeg_packet_pts(p: *const AVPacket) -> i64;
    fn moonfire_ffmpeg_packet_dts(p: *const AVPacket) -> i64;
    fn moonfire_ffmpeg_packet_duration(p: *const AVPacket) -> libc::c_int;
    fn moonfire_ffmpeg_packet_set_pts(p: *mut AVPacket, pts: i64);
    fn moonfire_ffmpeg_packet_set_dts(p: *mut AVPacket, dts: i64);
    fn moonfire_ffmpeg_packet_set_duration(p: *mut AVPacket, dur: libc::c_int);
    fn moonfire_ffmpeg_packet_data(p: *const AVPacket) -> DataLen;
    fn moonfire_ffmpeg_packet_stream_index(p: *const AVPacket) -> libc::c_uint;

    // avformat
    fn moonfire_ffmpeg_fctx_streams(ctx: *const AVFormatContext) -> StreamsLen;
    //fn moonfire_ffmpeg_fctx_open_write(ctx: *mut AVFormatContext,
    //                                   url: *const libc::c_char) -> libc::c_int;

    fn moonfire_ffmpeg_stream_codecpar(stream: *const AVStream) -> *const AVCodecParameters;
    fn moonfire_ffmpeg_stream_time_base(stream: *const AVStream) -> AVRational;

    // avutil
    fn moonfire_ffmpeg_frame_image_alloc(f: *mut AVFrame, dims: *const ImageDimensions)
                                         -> libc::c_int;
}

pub struct Ffmpeg {}

// No accessors here; seems reasonable to assume ABI stability of this simple struct.
#[repr(C)]
struct AVDictionaryEntry {
    key: *mut libc::c_char,
    value: *mut libc::c_char,
}

// Likewise, seems reasonable to assume this struct has a stable ABI.
#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct AVRational {
    pub num: libc::c_int,
    pub den: libc::c_int,
}

// No ABI stability assumption here; use heap allocation/deallocation and accessors only.
enum AVCodec {}
pub enum AVCodecContext {}
pub enum AVCodecParameters {}
enum AVDictionary {}
enum AVFormatContext {}
pub enum AVFrame {}
enum AVInputFormat {}
enum AVPacket {}
enum AVStream {}
enum SwsContext {}
enum SwsFilter {}

impl AVCodecContext {
    pub fn width(&self) -> libc::c_int { unsafe { moonfire_ffmpeg_cctx_width(self) } }
    pub fn height(&self) -> libc::c_int { unsafe { moonfire_ffmpeg_cctx_height(self) } }
    pub fn pix_fmt(&self) -> PixelFormat {
        PixelFormat(unsafe { moonfire_ffmpeg_cctx_pix_fmt(self) })
    }
    pub fn codec_id(&self) -> CodecId {
        CodecId(unsafe { moonfire_ffmpeg_cctx_codec_id(self) })
    }
    pub fn codec_type(&self) -> MediaType {
        MediaType(unsafe { moonfire_ffmpeg_cctx_codec_type(self) })
    }
    pub fn params(&self) -> VideoParameters {
        let mut p = std::mem::MaybeUninit::uninit();
        unsafe {
            moonfire_ffmpeg_cctx_params(self, p.as_mut_ptr());
            p.assume_init()
        }
    }
}

pub struct InputFormatContext {
    ctx: *mut AVFormatContext,
    pkt: RefCell<*mut AVPacket>,
}

impl InputFormatContext {
    pub fn open(source: &CStr, dict: &mut Dictionary) -> Result<Self, Error> {
        let mut ctx = ptr::null_mut();
        Error::wrap(unsafe {
            avformat_open_input(&mut ctx, source.as_ptr(), ptr::null(), &mut dict.0)
        })?;
        let pkt = unsafe { moonfire_ffmpeg_packet_alloc() };
        if pkt.is_null() {
            panic!("malloc failed");
        }
        unsafe { av_init_packet(pkt) };
        Ok(InputFormatContext{
            ctx,
            pkt: RefCell::new(pkt),
        })
    }

    pub fn find_stream_info(&mut self) -> Result<(), Error> {
        Error::wrap(unsafe { avformat_find_stream_info(self.ctx, ptr::null_mut()) })?;
        Ok(())
    }

    // XXX: non-mut because of lexical lifetime woes in the caller. This is also why we need a
    // RefCell.
    pub fn read_frame<'i>(&'i self) -> Result<Packet<'i>, Error> {
        let pkt = self.pkt.borrow();
        Error::wrap(unsafe { av_read_frame(self.ctx, *pkt) })?;
        Ok(Packet(pkt))
    }

    pub fn streams<'i>(&'i self) -> Streams<'i> {
        Streams(unsafe {
            let s = moonfire_ffmpeg_fctx_streams(self.ctx);
            std::slice::from_raw_parts(s.streams, s.len as usize)
        })
    }
}

unsafe impl Send for InputFormatContext {}

impl Drop for InputFormatContext {
    fn drop(&mut self) {
        unsafe {
            moonfire_ffmpeg_packet_free(*self.pkt.borrow());
            avformat_close_input(&mut self.ctx);
        }
    }
}

// matches moonfire_ffmpeg_data_len
#[repr(C)]
struct DataLen {
    data: *const u8,
    len: libc::size_t,
}

#[repr(C)]
struct FrameStuff {
    dims: ImageDimensions,
    data: *const *mut u8,
    linesizes: *const libc::c_int,
}

// matches moonfire_ffmpeg_streams_len
#[repr(C)]
struct StreamsLen {
    streams: *const *const AVStream,
    len: libc::size_t,
}

pub struct Packet<'i>(Ref<'i, *mut AVPacket>);

impl<'i> Packet<'i> {
    pub fn is_key(&self) -> bool { unsafe { moonfire_ffmpeg_packet_is_key(*self.0) } }
    pub fn pts(&self) -> Option<i64> {
        match unsafe { moonfire_ffmpeg_packet_pts(*self.0) } {
            v if v == unsafe { moonfire_ffmpeg_av_nopts_value } => None,
            v => Some(v),
        }
    }
    pub fn set_pts(&mut self, pts: Option<i64>) {
        let real_pts = match pts {
            None => unsafe { moonfire_ffmpeg_av_nopts_value },
            Some(v) => v,
        };
        unsafe { moonfire_ffmpeg_packet_set_pts(*self.0, real_pts); }
    }
    pub fn dts(&self) -> i64 { unsafe { moonfire_ffmpeg_packet_dts(*self.0) } }
    pub fn set_dts(&mut self, dts: i64) {
        unsafe { moonfire_ffmpeg_packet_set_dts(*self.0, dts); }
    }
    pub fn duration(&self) -> i32 { unsafe { moonfire_ffmpeg_packet_duration(*self.0) } }
    pub fn set_duration(&mut self, dur: i32) {
        unsafe { moonfire_ffmpeg_packet_set_duration(*self.0, dur) }
    }
    pub fn stream_index(&self) -> usize {
        unsafe { moonfire_ffmpeg_packet_stream_index(*self.0) as usize }
    }
    pub fn data(&self) -> Option<&[u8]> {
        unsafe {
            let d = moonfire_ffmpeg_packet_data(*self.0);
            if d.data.is_null() {
                None
            } else {
                Some(::std::slice::from_raw_parts(d.data, d.len))
            }
        }
    }
}

impl<'i> Drop for Packet<'i> {
    fn drop(&mut self) {
        unsafe {
            av_packet_unref(*self.0);
        }
    }
}

pub struct Streams<'owner>(&'owner [*const AVStream]);

impl<'owner> Streams<'owner> {
    pub fn get(&self, i: usize) -> InputStream<'owner> {
        InputStream(unsafe { self.0[i].as_ref() }.unwrap())
    }
    pub fn len(&self) -> usize { self.0.len() }
}

impl AVCodecParameters {
    pub fn extradata(&self) -> &[u8] {
        unsafe {
            let d = moonfire_ffmpeg_codecpar_extradata(self);
            ::std::slice::from_raw_parts(d.data, d.len)
        }
    }
    pub fn dims(&self) -> ImageDimensions {
        assert!(self.codec_type().is_video());
        unsafe { moonfire_ffmpeg_codecpar_dims(self) }
    }
    pub fn codec_id(&self) -> CodecId {
        CodecId(unsafe { moonfire_ffmpeg_codecpar_codec_id(self) })
    }
    pub fn codec_type(&self) -> MediaType {
        MediaType(unsafe { moonfire_ffmpeg_codecpar_codec_type(self) })
    }
}

pub struct InputStream<'o>(&'o AVStream);

impl<'o> InputStream<'o> {
    pub fn codecpar<'s>(&'s self) -> InputCodecParameters<'s> {
        InputCodecParameters(unsafe { moonfire_ffmpeg_stream_codecpar(self.0).as_ref() }.unwrap())
    }

    pub fn time_base(&self) -> AVRational {
        unsafe { moonfire_ffmpeg_stream_time_base(self.0) }
    }
}

pub struct InputCodecParameters<'s>(&'s AVCodecParameters);

impl<'s> InputCodecParameters<'s> {
    /*pub fn extradata(&self) -> &[u8] {
        unsafe {
            let d = moonfire_ffmpeg_cctx_extradata(self.0);
            ::std::slice::from_raw_parts(d.data, d.len)
        }
    }*/

    pub fn new_decoder(&self, options: &mut Dictionary) -> Result<DecodeContext, Error> {
        let decoder = match self.codec_id().find_decoder() {
            Some(d) => d,
            None => { return Err(Error::decoder_not_found()); },
        };
        let mut c = decoder.alloc_context()?;
        Error::wrap(unsafe { avcodec_parameters_to_context(c.ctx.as_ptr(), self.0) })?;
        c.open(options)?;
        Ok(c)
    }
}

impl<'s> std::ops::Deref for InputCodecParameters<'s> {
    type Target = AVCodecParameters;
    fn deref(&self) -> &AVCodecParameters { self.0 }
}

#[derive(Copy, Clone, Debug)]
pub struct CodecId(libc::c_int);

impl CodecId {
    pub fn is_h264(self) -> bool { self.0 == unsafe { moonfire_ffmpeg_av_codec_id_h264 } }

    pub fn find_decoder(self) -> Option<Decoder> {
        // avcodec_find_decoder returns an AVCodec which lives forever.
        unsafe { avcodec_find_decoder(self.0).as_ref() }.map(|d| Decoder(d))
    }

    pub fn find_encoder(self) -> Option<Encoder> {
        // avcodec_find_encoder returns an AVCodec which lives forever.
        unsafe { avcodec_find_encoder(self.0).as_ref() }.map(|e| Encoder(e))
    }
}

#[derive(Copy, Clone)]
pub struct Decoder(&'static AVCodec);

impl Decoder {
    fn alloc_context(self) -> Result<DecodeContext, Error> {
        let ctx = ptr::NonNull::new(unsafe { avcodec_alloc_context3(self.0) })
            .ok_or_else(Error::enomem)?;
        Ok(DecodeContext {
            decoder: self,
            ctx,
        })
    }
}

pub struct DecodeContext {
    decoder: Decoder,
    ctx: ptr::NonNull<AVCodecContext>,
}

impl Drop for DecodeContext {
    fn drop(&mut self) {
        let mut ctx = self.ctx.as_ptr();
        unsafe { avcodec_free_context(&mut ctx) }
    }
}

impl DecodeContext {
    fn open(&mut self, options: &mut Dictionary) -> Result<(), Error> {
        Error::wrap(unsafe { avcodec_open2(self.ctx.as_mut(), self.decoder.0, &mut options.0) })?;
        Ok(())
    }

    pub fn ctx(&self) -> &AVCodecContext { unsafe { self.ctx.as_ref() } }

    pub fn decode_video(&self, pkt: &Packet, frame: &mut VideoFrame) -> Result<bool, Error> {
        let mut got_picture: libc::c_int = 0;
        Error::wrap(unsafe {
            avcodec_decode_video2(self.ctx.as_ptr(), frame.frame.as_mut(), &mut got_picture, *pkt.0)
        })?;
        if got_picture != 0 {
            unsafe { moonfire_ffmpeg_frame_stuff(frame.frame.as_ptr(), &mut frame.stuff) };
            return Ok(true);
        };
        Ok(false)
    }
}

#[derive(Copy, Clone)]
pub struct Encoder(&'static AVCodec);

impl Encoder {
    /*pub fn alloc_context(self) -> Result<EncodeContext, Error> {
        let ctx = unsafe { avcodec_alloc_context3(self.0) };
        if ctx.is_null() {
            return Err(Error::enomem());
        }
        Ok(EncodeContext {
            encoder: self,
            ctx,
        })
    }*/
}

pub struct EncodeContext<'a>(&'a mut AVCodecContext);

/*impl Drop for EncodeContext {
    fn drop(&mut self) {
        unsafe { avcodec_free_context(&mut self.ctx) }
    }
}*/

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct VideoParameters {
    width: libc::c_int,
    height: libc::c_int,
    sample_aspect_ratio: AVRational,
    pix_fmt: PixelFormat,
    time_base: AVRational,
}

impl<'a> EncodeContext<'a> {
    pub fn set_params(&mut self, p: &VideoParameters) {
        unsafe { moonfire_ffmpeg_cctx_set_params(self.0, p) };
    }

    pub fn open(&mut self, encoder: Encoder, options: &mut Dictionary) -> Result<(), Error> {
        Error::wrap(unsafe { avcodec_open2(self.0, encoder.0, &mut options.0) })?;
        Ok(())
    }
}

#[derive(Copy, Clone, Debug)]
pub struct MediaType(libc::c_int);

impl MediaType {
    pub fn is_video(self) -> bool { self.0 == unsafe { moonfire_ffmpeg_avmedia_type_video } }
}

pub struct VideoFrame {
    frame: ptr::NonNull<AVFrame>,
    stuff: FrameStuff,
}

pub struct Plane<'f> {
    pub data: &'f [u8],
    pub linesize: usize,
    pub width: usize,
    pub height: usize,
}

impl VideoFrame {
    /// Creates a new `VideoFrame` which is empty: no allocated storage (reference-counted or
    /// otherwise). Can be filled via `DecodeContexst::decode_video`.
    pub fn empty() -> Result<Self, Error> {
        let frame = ptr::NonNull::new(unsafe { av_frame_alloc() }).ok_or_else(Error::enomem)?;
        Ok(VideoFrame {
            frame,
            stuff: FrameStuff {
                dims: ImageDimensions {
                    width: 0,
                    height: 0,
                    pix_fmt: PixelFormat(-1),
                },
                data: ptr::null(),
                linesizes: ptr::null(),
            },
        })
    }

    /// Creates a new `VideoFrame` with an owned (not reference-counted) buffer of the specified
    /// dimensions.
    pub fn owned(dims: ImageDimensions) -> Result<Self, Error> {
        let mut frame = VideoFrame::empty()?;
        Error::wrap(unsafe { moonfire_ffmpeg_frame_image_alloc(frame.frame.as_mut(), &dims) })?;
        unsafe { moonfire_ffmpeg_frame_stuff(frame.frame.as_ptr(), &mut frame.stuff) };
        Ok(frame)
    }

    pub fn plane(&self, plane: usize) -> Plane {
        assert!(plane < 8);
        let d = unsafe { *self.stuff.data.offset(plane as isize) };
        let l = unsafe { *self.stuff.linesizes.offset(plane as isize) };
        assert!(!d.is_null());
        assert!(l > 0);
        let l = l as usize;
        let width = self.stuff.dims.width as usize;
        let height = self.stuff.dims.height as usize;
        Plane {
            data: unsafe { std::slice::from_raw_parts(d, l * height) } ,
            linesize: l,
            width,
            height,
        }
    }

    pub fn dims(&self) -> ImageDimensions { self.stuff.dims }
}

impl Drop for VideoFrame {
    fn drop(&mut self) {
        unsafe {
            let mut f = self.frame.as_ptr();
            av_frame_free(&mut f);
            // This leaves self.frame dangling, but it's being dropped.
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq)]
#[repr(transparent)]
pub struct PixelFormat(libc::c_int);

impl fmt::Debug for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "PixelFormat({} /* {} */)", self.0, self)
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = unsafe {
            let n = av_get_pix_fmt_name(self.0);
            if n.is_null() {
                return write!(f, "PixelFormat({})", self.0);
            }
            CStr::from_ptr(n)
        };
        f.write_str(&s.to_string_lossy())
    }
}

// Must match moonfire_ffmpeg_image_dimensions.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
pub struct ImageDimensions {
    pub width: libc::c_int,
    pub height: libc::c_int,
    pub pix_fmt: PixelFormat,
}

impl fmt::Display for ImageDimensions {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}x{}/{}", self.width, self.height, self.pix_fmt)
    }
}

pub struct Scaler {
    ctx: ptr::NonNull<SwsContext>,
    src: ImageDimensions,
    dst: ImageDimensions,
}

impl Scaler {
    pub fn new(src: ImageDimensions, dst: ImageDimensions) -> Result<Self, Error> {
        // TODO: yuvj420p causes an annoying warning "deprecated pixel format used, make sure you
        // did set range correctly" here. Looks like we need to change to yuv420p and call
        // sws_setColorspaceDetails to get the same effect while suppressing this warning.
        println!("scaler for {} -> {}", src, dst);
        let ctx = ptr::NonNull::new(unsafe {
            sws_getContext(src.width, src.height, src.pix_fmt.0, dst.width, dst.height,
                           dst.pix_fmt.0, moonfire_ffmpeg_sws_bilinear, ptr::null_mut(),
                           ptr::null_mut(), ptr::null())
        }).ok_or_else(Error::unknown)?;
        Ok(Scaler {
            ctx,
            src,
            dst,
        })
    }

    pub fn scale(&mut self, src: &VideoFrame, dst: &mut VideoFrame) {
        assert_eq!(src.dims(), self.src);
        assert_eq!(dst.dims(), self.dst);
        unsafe {
            sws_scale(self.ctx.as_ptr(), src.stuff.data.cast(),
                      src.stuff.linesizes, 0, self.src.height, dst.stuff.data, dst.stuff.linesizes)
        };
    }
}

impl Drop for Scaler {
    fn drop(&mut self) { unsafe { sws_freeContext(self.ctx.as_ptr()) } }
}

#[derive(Copy, Clone)]
pub struct Error(libc::c_int);

impl Error {
    pub fn eof() -> Self { Error(unsafe { moonfire_ffmpeg_averror_eof }) }
    pub fn enomem() -> Self { Error(unsafe { moonfire_ffmpeg_averror_enomem }) }
    pub fn unknown() -> Self { Error(unsafe { moonfire_ffmpeg_averror_unknown }) }
    pub fn decoder_not_found() -> Self {
        Error(unsafe { moonfire_ffmpeg_averror_decoder_not_found })
    }

    fn wrap(raw: libc::c_int) -> Result<libc::c_int, Error> {
        if raw < 0 {
            return Err(Error(raw));
        }
        Ok(raw)
    }

    pub fn is_eof(self) -> bool { return self.0 == unsafe { moonfire_ffmpeg_averror_eof } }
}

impl std::error::Error for Error {}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Error({} /* {} */)", self.0, self)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        const ARRAYLEN: usize = 64;
        let mut buf = [0; ARRAYLEN];
        let s = unsafe {
            // Note av_strerror uses strlcpy, so it guarantees a trailing NUL byte.
            av_strerror(self.0, buf.as_mut_ptr(), ARRAYLEN);
            CStr::from_ptr(buf.as_ptr())
        };
        f.write_str(&s.to_string_lossy())
    }
}

#[derive(Copy, Clone)]
struct Version(libc::c_int);

impl Version {
    fn major(self) -> libc::c_int { (self.0 >> 16) & 0xFF }
    fn minor(self) -> libc::c_int { (self.0 >> 8) & 0xFF }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}.{}.{}", (self.0 >> 16) & 0xFF, (self.0 >> 8) & 0xFF, self.0 & 0xFF)
    }
}

struct Library {
    name: &'static str,
    compiled: Version,
    running: Version,
}

impl Library {
    fn new(name: &'static str, compiled: libc::c_int, running: libc::c_int) -> Self {
        Library {
            name,
            compiled: Version(compiled),
            running: Version(running),
        }
    }

    fn is_compatible(&self) -> bool {
        self.running.major() == self.compiled.major() &&
            self.running.minor() >= self.compiled.minor()
    }
}

impl fmt::Display for Library {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: running={} compiled={}", self.name, self.running, self.compiled)
    }
}

pub struct Dictionary(*mut AVDictionary);

impl Dictionary {
    pub fn new() -> Dictionary { Dictionary(ptr::null_mut()) }
    pub fn size(&self) -> usize { (unsafe { av_dict_count(self.0) }) as usize }
    pub fn empty(&self) -> bool { self.size() == 0 }
    pub fn set(&mut self, key: &CStr, value: &CStr) -> Result<(), Error> {
        Error::wrap(unsafe { av_dict_set(&mut self.0, key.as_ptr(), value.as_ptr(), 0) })?;
        Ok(())
    }
}

impl fmt::Display for Dictionary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut ent = ptr::null_mut();
        let mut first = true;
        loop {
            unsafe {
                let c = 0;
                ent = av_dict_get(self.0, &c, ent, moonfire_ffmpeg_av_dict_ignore_suffix);
                if ent.is_null() {
                    break;
                }
                if first {
                    first = false;
                } else {
                    write!(f, ", ")?;
                }
                write!(f, "{}={}", CStr::from_ptr((*ent).key).to_string_lossy(),
                      CStr::from_ptr((*ent).value).to_string_lossy())?;
            }
        }
        Ok(())
    }
}

impl Drop for Dictionary {
    fn drop(&mut self) { unsafe { av_dict_free(&mut self.0) } }
}

impl Ffmpeg {
    pub fn new() -> Ffmpeg {
        START.call_once(|| unsafe {
            let libs = &[
                Library::new("avutil", moonfire_ffmpeg_compiled_libavutil_version,
                             avutil_version()),
                Library::new("avcodec", moonfire_ffmpeg_compiled_libavcodec_version,
                             avcodec_version()),
                Library::new("avformat", moonfire_ffmpeg_compiled_libavformat_version,
                             avformat_version()),
                Library::new("swscale", moonfire_ffmpeg_compiled_libswscale_version,
                             swscale_version()),
            ];
            let mut msg = String::new();
            let mut compatible = true;
            for l in libs {
                write!(&mut msg, "\n{}", l).unwrap();
                if !l.is_compatible() {
                    compatible = false;
                    msg.push_str(" <- not ABI-compatible!");
                }
            }
            if !compatible {
                panic!("Incompatible ffmpeg versions:{}", msg);
            }
            moonfire_ffmpeg_init();
            av_register_all();
            if avformat_network_init() < 0 {
                panic!("avformat_network_init failed");
            }
            info!("Initialized ffmpeg. Versions:{}", msg);
        });
        Ffmpeg{}
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use super::Error;

    /// Just tests that this doesn't crash with an ABI compatibility error.
    #[test]
    fn test_init() { super::Ffmpeg::new(); }

    #[test]
    fn test_is_compatible() {
        // compiled major/minor/patch, running major/minor/patch, expected compatible
        use ::libc::c_int;
        struct Test(c_int, c_int, c_int, c_int, c_int, c_int, bool);

        let tests = &[
            Test(55, 1, 2, 55, 1, 2, true),   // same version, compatible
            Test(55, 1, 2, 55, 2, 1, true),   // newer minor version, compatible
            Test(55, 1, 3, 55, 1, 2, true),   // older patch version, compatible (but weird)
            Test(55, 2, 2, 55, 1, 2, false),  // older minor version, incompatible
            Test(55, 1, 2, 56, 1, 2, false),  // newer major version, incompatible
            Test(56, 1, 2, 55, 1, 2, false),  // older major version, incompatible
        ];

        for t in tests {
            let l = super::Library::new(
                "avutil", (t.0 << 16) | (t.1 << 8) | t.2, (t.3 << 16) | (t.4 << 8) | t.5);
            assert!(l.is_compatible() == t.6, "{} expected={}", l, t.6);
        }
    }

    #[test]
    fn test_error() {
        let eof_formatted = format!("{}", Error::eof());
        assert!(eof_formatted.contains("End of file"), "eof formatted is: {}", eof_formatted);
        let eof_debug = format!("{:?}", Error::eof());
        assert!(eof_debug.contains("End of file"), "eof debug is: {}", eof_debug);

        // Errors should be round trippable to a CString. (This will fail if they contain NUL
        // bytes.)
        CString::new(eof_formatted).unwrap();
    }
}
