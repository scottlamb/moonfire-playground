// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2019 Scott Lamb <slamb@slamb.org>
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

use failure::{bail, format_err, Error};
use http::header::{self, HeaderMap, HeaderName, HeaderValue};
use httparse;
use mime;
use std::io::Read;

/// Maximum length of a boundary line and part headers within a multipart/mixed stream.
static MAX_HEADER_LEN: usize = 1024;

/// Maximum length of the body of a part within a multipart/mixed stream.
static MAX_BODY_LEN: u64 = 1024;

pub struct Part<'a> {
    pub headers: HeaderMap,
    pub body: &'a [u8],
}

/// Loops over each part in the given HTTP response.
///
/// # Arguments
///
/// * `expected_subtype` should be the expected multipart subtype: `mixed` or `x-mixed-replace`.
/// * `separator` is the newlines (if any) to expect between parts.
pub fn foreach_part<F>(r: &mut reqwest::Response, expected_subtype: &str, separator: &str,
                       f: F) -> Result<(), Error>
where F: FnMut(Part) -> Result<(), Error> {
    if r.status() != http::status::StatusCode::OK {
        bail!("non-okay status: {:?}", r.status());
    }
    let m: mime::Mime = r.headers().get(header::CONTENT_TYPE)
        .ok_or_else(|| format_err!("no content type header"))?
        .to_str()?
        .parse()?;
    foreach_part_inner(m, r, expected_subtype, separator, f)
}

fn foreach_part_inner<F>(content_type: mime::Mime, r: &mut Read, expected_subtype: &str,
                         separator: &str, mut f: F) -> Result<(), Error>
where F: FnMut(Part) -> Result<(), Error> {
    // Examine the headers: verify Content-Type is as expected, and determine the boundary.
    let boundary_buf = {
        if content_type.type_() != mime::MULTIPART || content_type.subtype() != expected_subtype {
            bail!("unknown content type {:?}", content_type);
        }
        let boundary = content_type.get_param(mime::BOUNDARY)
            .ok_or_else(|| format_err!("no boundary in mime {:?}", content_type))?;
        let mut line = Vec::with_capacity(separator.len() + boundary.as_str().len() + 4);
        line.extend_from_slice(separator.as_bytes());
        line.extend_from_slice(b"--");
        line.extend_from_slice(boundary.as_str().as_bytes());
        line.extend_from_slice(b"\r\n");
        line
    };
    let mut cur_boundary = &boundary_buf[separator.len()..];

    let mut buf = Vec::with_capacity(1024);

    while let Some((body_pos, headers)) = start_part(&mut buf, &cur_boundary, r)? {
        cur_boundary = &boundary_buf[..];
        let body_len: u64 = headers.get(header::CONTENT_LENGTH)
            .ok_or_else(|| format_err!("Missing part Content-Length"))?
            .to_str()?
            .parse()?;
        if body_len > MAX_BODY_LEN {
            bail!("body length {} exceeds maximum of {}", body_len, MAX_BODY_LEN);
        }
        let body_len = body_len as usize;
        let body_end = body_pos + body_len;
        if body_pos + body_len > buf.len() {
            let old_len = buf.len();
            buf.reserve(body_pos + body_len - old_len);

            // SAFE: this length is reserved, and buf is discarded on error.
            unsafe { buf.set_len(body_pos + body_len) };
            r.read_exact(&mut buf[old_len..])?;
        }

        f(Part {
            headers,
            body: &buf[body_pos .. body_end],
        })?;

        // Move the remainder to the start of the buffer for simplicity.
        buf.drain(0..body_end);
    }
    Ok(())
}

fn start_part(buf: &mut Vec<u8>, boundary: &[u8], r: &mut Read) -> Result<Option<(usize, HeaderMap)>, Error> {
    loop {
        let boundary_len = boundary.len();
        let have_boundary = {
            if buf.len() < boundary_len {
                false
            } else if !buf.starts_with(boundary) {
                use pretty_hex::PrettyHex;
                bail!("chunk does not start with expected boundary:\n{:?}\n\nchunk is:\n{:?}",
                      boundary.hex_dump(), buf.hex_dump());
            } else {
                true
            }
        };

        if have_boundary {
            // See if buf has complete headers, too.
            let mut raw = [httparse::EMPTY_HEADER; 16];
           match httparse::parse_headers(&buf.as_slice()[boundary_len..], &mut raw)? {
                httparse::Status::Complete((body_pos, raw)) => {
                    let mut headers = HeaderMap::with_capacity(raw.len());
                    for h in raw {
                        headers.append(HeaderName::from_bytes(h.name.as_bytes())?,
                                       HeaderValue::from_bytes(h.value)?);
                    }
                    return Ok(Some((boundary_len + body_pos, headers)));
                },
                httparse::Status::Partial => {},
            }
        }

        // Need to read more into buf.
        let old_len = buf.len();
        if old_len >= MAX_HEADER_LEN {
            bail!("part headers are longer than {} bytes", old_len);
        }
        buf.reserve(MAX_HEADER_LEN - old_len);

        // SAFE: this length is pre-reserved, and this code block always ensures the length matches
        // that used by r.read on exit (even if r.read fails).
        unsafe { buf.set_len(MAX_HEADER_LEN) };
        let additional = match r.read(&mut buf[old_len .. MAX_HEADER_LEN]) {
            Ok(a) => a,
            Err(e) => {
                buf.truncate(old_len);
                return Err(e.into());
            },
        };
        buf.truncate(old_len + additional);
        if buf.len() == 0 {
            return Ok(None);
        } else if additional == 0 {
            bail!("unexpected EOF while reading part boundary/headers");
        }
    };
}

#[cfg(test)]
mod tests {
    use super::foreach_part_inner;

    #[test]
    fn hikvision_style() {
        let input = concat!(
            "--boundary\r\n",
            "Content-Type: application/xml; charset=\"UTF-8\"\r\n",
            "Content-Length: 480\r\n",
            "\r\n",
            "<EventNotificationAlert version=\"1.0\" ",
            "xmlns=\"http://www.hikvision.com/ver10/XMLSchema\">\r\n",
            "<ipAddress>192.168.5.106</ipAddress>\r\n",
            "<portNo>80</portNo>\r\n",
            "<protocol>HTTP</protocol>\r\n",
            "<macAddress>8c:e7:48:da:94:8f</macAddress>\r\n",
            "<channelID>1</channelID>\r\n",
            "<dateTime>2019-02-20T15:22:34-8:00</dateTime>\r\n",
            "<activePostCount>0</activePostCount>\r\n",
            "<eventType>videoloss</eventType>\r\n",
            "<eventState>inactive</eventState>\r\n",
            "<eventDescription>videoloss alarm</eventDescription>\r\n",
            "</EventNotificationAlert>\r\n",
            "--boundary\r\n",
            "Content-Type: application/xml; charset=\"UTF-8\"\r\n",
            "Content-Length: 480\r\n",
            "\r\n",
            "<EventNotificationAlert version=\"1.0\" ",
            "xmlns=\"http://www.hikvision.com/ver10/XMLSchema\">\r\n",
            "<ipAddress>192.168.5.106</ipAddress>\r\n",
            "<portNo>80</portNo>\r\n",
            "<protocol>HTTP</protocol>\r\n",
            "<macAddress>8c:e7:48:da:94:8f</macAddress>\r\n",
            "<channelID>1</channelID>\r\n",
            "<dateTime>2019-02-20T15:22:34-8:00</dateTime>\r\n",
            "<activePostCount>0</activePostCount>\r\n",
            "<eventType>videoloss</eventType>\r\n",
            "<eventState>inactive</eventState>\r\n",
            "<eventDescription>videoloss alarm</eventDescription>\r\n",
            "</EventNotificationAlert>\r\n");
        let mut r = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut i = 0;
        foreach_part_inner("multipart/mixed; boundary=boundary".parse().unwrap(),
                           &mut r, "mixed", "", |p| {
            assert_eq!(p.headers.get(http::header::CONTENT_TYPE).unwrap().to_str().unwrap(),
                       "application/xml; charset=\"UTF-8\"");
            assert!(p.body.starts_with(b"<EventNotificationAlert"));
            assert!(p.body.ends_with(b"</EventNotificationAlert>\r\n"));
            i += 1;
            Ok(())
        }).unwrap();
        assert_eq!(i, 2);
    }

    #[test]
    fn dahua_style() {
        let input = concat!(
            "--myboundary\r\n",
            "Content-Type: text/plain\r\n",
            "Content-Length:135\r\n",
            "\r\n",
            "Code=TimeChange;action=Pulse;index=0;data={\n",
            "   \"BeforeModifyTime\" : \"2019-02-20 13:49:58\",\n",
            "   \"ModifiedTime\" : \"2019-02-20 13:49:58\"\n",
            "}\n",
            "\r\n",
            "\r\n",
            "--myboundary\r\n",
            "Content-Type: text/plain\r\n",
            "Content-Length:137\r\n",
            "\r\n",
            "Code=NTPAdjustTime;action=Pulse;index=0;data={\n",
            "   \"Address\" : \"192.168.5.254\",\n",
            "   \"Before\" : \"2019-02-20 13:49:57\",\n",
            "   \"result\" : true\n",
            "}\n");
        let mut r = std::io::Cursor::new(input.as_bytes().to_vec());
        let mut i = 0;
        foreach_part_inner("multipart/x-mixed-replace; boundary=myboundary".parse().unwrap(),
                           &mut r, "x-mixed-replace", "\r\n\r\n", |p| {
            assert_eq!(p.headers.get(http::header::CONTENT_TYPE).unwrap().to_str().unwrap(),
                       "text/plain");
            match i {
                0 => assert!(p.body.starts_with(b"Code=TimeChange")),
                1 => assert!(p.body.starts_with(b"Code=NTPAdjustTime")),
                _ => unreachable!(),
            }
            i += 1;
            Ok(())
        }).unwrap();
        assert_eq!(i, 2);
    }
}
