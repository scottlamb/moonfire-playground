//! Analysis of NAL units.
//! Currently this just counts VCL vs non-VCL bytes in a sample file from Moonfire NVR.
//! A sample file is a bunch of NAL units in AVC format with 4-byte length
//! prefixes. There's no timing information but that's fine.
//!
//! https://github.com/scottlamb/moonfire-nvr/issues/43
//!
//! See [ISO/IEC 14496-10:2014(E)](https://github.com/scottlamb/moonfire-nvr/wiki/Standards-and-specifications#video-codecs)
//! to understand the references here.

use failure::{Error, bail};
//use pretty_hex::PrettyHex;
use std::{convert::TryFrom, io::BufReader, io::Read};
use structopt::StructOpt;

#[derive(StructOpt)]
struct Opt {
    #[structopt(long, parse(from_os_str))]
    file: std::path::PathBuf,
}

#[derive(Debug)]
struct NalType {
    name: &'static str,
    is_vcl: bool,
}

// See Table 7-1, PDF page 85.
const NAL_TYPES: [Option<NalType>; 32] = [
    /*  0 */ None,
    /*  1 */ Some(NalType { name: "slice_layer_without_partitioning", is_vcl: true }),
    /*  2 */ Some(NalType { name: "slice_data_partition_a_layer",     is_vcl: true }),
    /*  3 */ Some(NalType { name: "slice_data_partition_b_layer",     is_vcl: true }),
    /*  4 */ Some(NalType { name: "slice_data_partition_c_layer",     is_vcl: true }),
    /*  5 */ Some(NalType { name: "slice_layer_without_partitioning", is_vcl: true }),
    /*  6 */ Some(NalType { name: "sei",                              is_vcl: false }),
    /*  7 */ Some(NalType { name: "seq_parameter_set",                is_vcl: false }),
    /*  8 */ Some(NalType { name: "pic_parameter_set",                is_vcl: false }),
    /*  9 */ Some(NalType { name: "access_unit_delimiter",            is_vcl: false }),
    /* 10 */ Some(NalType { name: "end_of_seq",                       is_vcl: false }),
    /* 11 */ Some(NalType { name: "end_of_stream",                    is_vcl: false }),
    /* 12 */ Some(NalType { name: "filler_data",                      is_vcl: false }),
    /* 13 */ Some(NalType { name: "seq_parameter_set_extension",      is_vcl: false }),
    /* 14 */ Some(NalType { name: "prefix_nal_unit",                  is_vcl: false }),
    /* 15 */ Some(NalType { name: "subset_seq_parameter_set",         is_vcl: false }),
    /* 16 */ Some(NalType { name: "depth_parameter_set",              is_vcl: false }),
    /* 17 */ None,
    /* 18 */ None,
    /* 19 */ Some(NalType { name: "slice_layer_without_partitioning", is_vcl: false }),
    /* 20 */ Some(NalType { name: "slice_layer_extension",            is_vcl: false }),
    /* 21 */ Some(NalType { name: "slice_layer_extension_for_3d",     is_vcl: false }),
    /* 22 */ None,
    /* 23 */ None,
    /* 24 */ None,
    /* 25 */ None,
    /* 26 */ None,
    /* 27 */ None,
    /* 28 */ None,
    /* 29 */ None,
    /* 30 */ None,
    /* 31 */ None,
];

fn main() -> Result<(), Error> {
    let opt = Opt::from_args();
    let f = std::fs::File::open(opt.file)?;
    let len = f.metadata()?.len();
    let mut reader = BufReader::with_capacity(1 << 10, f);
    let mut pos = 0;
    let mut nal_buf = Vec::with_capacity(1 << 10);
    let mut vcl_len = 0;
    let mut non_vcl_len = 0;
    while pos < len {
        // Read the next NAL into nal_buf and advance pos.
        if pos + 4 >= len {
            bail!("{} bytes left at end; expected 4-byte len", len - pos);
        }
        let mut len_buf = [0u8; 4];
        reader.read_exact(&mut len_buf[..])?;
        let nal_len = u32::from_be_bytes(len_buf);
        pos = pos.checked_add(4).unwrap();
        let nal_len_usize = usize::try_from(nal_len)?;
        let nal_len_u64 = u64::from(nal_len);
        let end_pos = pos.checked_add(nal_len_u64).unwrap();
        if end_pos > len {
            bail!("nal too large");
        }
        nal_buf.resize(nal_len_usize, 0);
        reader.read_exact(&mut nal_buf)?;
        pos = pos.checked_add(u64::from(nal_len)).unwrap();

        // Process nal_buf.
        if nal_buf.is_empty() {
            bail!("Empty NAL");
        }
        let nal_type_u8 = nal_buf[0] & 0x1F;
        let nal_type = NAL_TYPES[usize::try_from(nal_type_u8).unwrap()].as_ref().unwrap();
        match nal_type.is_vcl {
            false => {
                println!("{:?}", nal_type);
                non_vcl_len += 4 + nal_len_u64;
            },
            true => vcl_len += 4 + nal_len_u64,
        }
    }
    println!("non_vcl_len: {}\nvcl_len: {}", non_vcl_len, vcl_len);
    Ok(())
}