//! Analysis of AVC configuration data.
//!
//! My current goal is to see how what upper bound we can place on the bitrate of the received
//! stream for [moonfire-nvr#116](https://github.com/scottlamb/moonfire-nvr/issues/116). There's probably
//! some other interesting things in the SPS/PPS we're not using today, like the frame rate.
//!
//! See [ISO/IEC 14496-10:2014(E)](https://github.com/scottlamb/moonfire-nvr/wiki/Standards-and-specifications#video-codecs)
//! to understand the references here.
//!
//! If we can determine the bitrate accurately, we might put the logic for doing so into convenient methods
//! in the `h264-reader` crate. See [h264-reader#9](https://github.com/dholroyd/h264-reader/issues/9).

use failure::Error;
//use pretty_hex::PrettyHex;
use rusqlite::params;
use std::convert::{TryFrom, TryInto};
use structopt::StructOpt;

#[derive(StructOpt)]
struct Opt {
    #[structopt(long, parse(from_os_str))]
    db: std::path::PathBuf,
}

/// Determine the max bitrate (a far upper bound) from the profile and level from Table A-1.
/// Multiply by 1000 bits/sec for VCL bitstreams or 1200 bits/sec for NAL bitstreams.
fn level_maxbr(sps: &h264_reader::nal::sps::SeqParameterSet) -> u32 {
    let profile_idc = u8::from(sps.profile_idc);
    // Is this applicable to the high profile (profile_idc = 100)? Not entirely sure.
    //assert!([66, 77, 88].contains(&profile_idc), "profile_idc={}", profile_idc);
    // Table A-1 on PDF page 329 (page 309, according to the footer).
    match (sps.level_idc, sps.constraint_flags.flag3()) {
        (10, _)     =>      64, // level 1
        (11, true)  =>     128, // level 1b
        (11, false) =>     192, // level 1.1
        (12, _)     =>     384, // level 1.2
        (13, _)     =>     768, // level 1.3
        (20, _)     =>   2_000, // level 2
        (21, _)     =>   4_000, // level 2.1
        (22, _)     =>   4_000, // level 2.2
        (30, _)     =>  10_000, // level 3
        (31, _)     =>  14_000, // level 3.1
        (32, _)     =>  20_000, // level 3.2
        (40, _)     =>  20_000, // level 4
        (41, _)     =>  50_000, // level 4.1
        (42, _)     =>  50_000, // level 4.2
        (50, _)     => 135_000, // level 5
        (51, _)     => 240_000, // level 5.1
        (52, _)     => 240_000, // level 5.2

        // TODO: Are values not specified in the table allowed? one assumes so?
        (o, _)      => panic!("unknown level_idc {}", o),
    }
}

/// Determine the bitrate from the VUI parameters.
/// This is a much tighter bound, but it doesn't seem to always be present.
fn hrd_br(sps: &h264_reader::nal::sps::SeqParameterSet) -> Option<u32> {
    // See E.2.2 on PDF page 404 (page 384 according to the footer).
    // There are two sets of HRD parameters: the "NAL" ones and the "VCL" ones.
    // The difference seems to be that the "NAL" ones may specify a slightly higher bitrate to allow
    // for extra included NAL units. Currently Moonfire NVR includes these in its sample data, so it's
    // most appropriate to use the NAL ones. We could change this, as described in the isuse below.
    // I don't think it really makes the same 20% difference in actual bitrate as it does in the max level bitrate.
    // See https://github.com/scottlamb/moonfire-nvr/issues/43.
    let hrd = match sps.vui_parameters.as_ref().and_then(|v| v.nal_hrd_parameters.as_ref()) {
        None => return None,
        Some(h) => h,
    };
    let cpb_spec = match hrd.cpb_specs.first() {
        None => return None,
        Some(c) => c,
    };
    Some((cpb_spec.bit_rate_value_minus1 + 1) << (6 + hrd.bit_rate_scale))
}

fn process_vse(id: i32, sample_entry: &[u8]) -> Result<(), Error> {
    // Assume sample_entry is written by moonfire-nvr's server/src/h264.rs. It contains an avc1 box, version 0,
    // short length. The magic number 86 below is based on this assumption.
    //println!("{}\n{:?}\n", id, sample_entry.hex_dump());
    assert!(&sample_entry[4 .. 8] == b"avc1");
    const AVCC_BOX_START: usize = 86;
    assert!(&sample_entry[AVCC_BOX_START+4 .. AVCC_BOX_START+8] == b"avcC");
    let avcc_len = u32::from_be_bytes(sample_entry[AVCC_BOX_START .. AVCC_BOX_START+4].try_into()?);
    let avcc_len = usize::try_from(avcc_len)?;
    let config = &sample_entry[AVCC_BOX_START+8 .. AVCC_BOX_START+avcc_len];
    let config = h264_reader::avcc::AvcDecoderConfigurationRecord::try_from(config).unwrap();
    let ctx = config.create_context(()).unwrap();
    let sps = ctx
        .sps_by_id(h264_reader::nal::pps::ParamSetId::from_u32(0).unwrap())
        .unwrap();
    println!("{}: {:#?}", id, &sps);
    let nal_level_br = level_maxbr(sps) * 1200;
    let hrd_br = hrd_br(sps);
    println!("{}: hrd_br={:?}, nal_level_br={}", id, hrd_br, nal_level_br);
    if let Some(h) = hrd_br {
        assert!(h <= nal_level_br);
    }
    Ok(())
}

fn main() -> Result<(), Error> {
    let opt = Opt::from_args();
    let conn = rusqlite::Connection::open_with_flags(&opt.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut stmt = conn.prepare("select id, rfc6381_codec, data from video_sample_entry")?;
    let mut rows = stmt.query(params![])?;
    while let Some(row) = rows.next()? {
        let id: i32 = row.get(0)?;
        //let rfc6381_codec: String = row.get(1)?;
        let sample_entry: Vec<u8> = row.get(2)?;
        process_vse(id, &sample_entry)?;
    }
    Ok(())
}