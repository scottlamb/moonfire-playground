// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 Scott Lamb <slamb@slamb.org>
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Multipart stream parser.

use failure::{bail, format_err, Error};
use futures::Stream;
use mime;

pub(crate) use multipart_stream::Part;

/// Returns a stream of `Part`s in the given HTTP response.
///
/// # Arguments
///
/// * `expected_subtype` should be the expected multipart subtype: `mixed` or `x-mixed-replace`.
pub fn parse(r: reqwest::Response, expected_subtype: &str)
    -> Result<impl Stream<Item = Result<multipart_stream::Part, multipart_stream::parser::Error>>, Error> {
    if r.status() != reqwest::StatusCode::OK {
        bail!("non-okay status: {:?}", r.status());
    }
    let content_type: mime::Mime = r.headers().get(reqwest::header::CONTENT_TYPE)
        .ok_or_else(|| format_err!("no content type header"))?
        .to_str()?
        .parse()?;
    if content_type.type_() != mime::MULTIPART || content_type.subtype() != expected_subtype {
        bail!("unknown content type {:?}", content_type);
    }
    let boundary = content_type.get_param(mime::BOUNDARY)
        .ok_or_else(|| format_err!("no boundary in mime {:?}", content_type))?
        .as_str();
    Ok(multipart_stream::ParserBuilder::new()
        .max_header_bytes(1024)
        .max_body_bytes(1024)
        .parse(r.bytes_stream(), boundary))
}

