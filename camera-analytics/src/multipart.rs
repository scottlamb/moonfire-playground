// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 Scott Lamb <slamb@slamb.org>
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Multipart stream parser.
//! Multipart streams are explained in [the README for my Javascript
//! parser](https://github.com/scottlamb/multipart-stream-js).
//! 
//! This implementation is pretty gross (it's hard to read and does more copying than necessary)
//! due to a combination of the following:
//! 
//! 1.  the current state of Rust async: in particular, that there are no coroutines.
//! 2.  my inexperience with Rust async
//! 3.  how quickly I threw this together.
//!
//! Fortunately the badness is hidden behind a decent interface, and there are decent tests
//! of success cases with partial data. In the situations we're using it (small
//! bits of metadata rather than video), the inefficient probably doesn't matter.
//! TODO: add tests of bad inputs.

use bytes::{Buf, Bytes, BytesMut};
use failure::{bail, format_err, Error};
use futures::Stream;
use reqwest::header::{self, HeaderMap, HeaderName, HeaderValue};
use httparse;
use mime;
use pin_project::pin_project;
use std::{pin::Pin, task::{Context, Poll}};

/// Maximum length of part headers within a `multipart/mixed` stream.
static MAX_HEADER_LEN: usize = 1024;

/// Maximum length of the body of a part within a `multipart/mixed` stream.
static MAX_BODY_LEN: usize = 1024;

pub struct Part {
    pub headers: HeaderMap,
    pub body: Bytes,
}

#[pin_project]
pub struct Parts<S> where S: Stream<Item = Result<Bytes, reqwest::Error>> {
    #[pin]
    input: S,
    boundary: Vec<u8>,
    buf: BytesMut,
    state: State,
}

enum State {
    /// Waiting for the completion of a boundary.
    /// `pos` is the current offset within `boundary_buf`.
    Boundary { pos: usize },

    /// Waiting for a full set of headers.
    Headers,

    /// Waiting for a full body.
    Body { headers: HeaderMap, body_len: usize },

    /// The stream is finished (has returned error).
    Done,
}

impl State {
    /// Processes the current buffer contents.
    /// This reverses the order of the return value so it can return error via `?` and `bail!`.
    /// The caller puts it back into the order expected by `Stream`.
    fn process(&mut self, boundary: &[u8], buf: &mut BytesMut) -> Result<Poll<Option<Part>>, Error> {
        loop {
            match self {
                State::Boundary { ref mut pos } => {
                    let len = std::cmp::min(boundary.len() - *pos, buf.len());
                    if buf[0 .. len] != boundary[*pos .. *pos + len] {
                        use pretty_hex::PrettyHex;
                        return Err(format_err!(
                            "chunk does not match expected boundary, from pos {}, \
                            {:?} vs (entire) expected: {:?}", *pos,
                            (&buf[0 .. len]).hex_dump(), boundary.hex_dump()));
                    }
                    buf.advance(len);
                    *pos += len;
                    if *pos < boundary.len() {
                        return Ok(Poll::Pending);
                    } else {
                        *self = State::Headers;
                    }
                },
                State::Headers => {
                    let mut raw = [httparse::EMPTY_HEADER; 16];
                    match httparse::parse_headers(&buf, &mut raw)? {
                        httparse::Status::Complete((body_pos, raw)) => {
                            let mut headers = HeaderMap::with_capacity(raw.len());
                            for h in raw {
                                headers.append(HeaderName::from_bytes(h.name.as_bytes())?,
                                            HeaderValue::from_bytes(h.value)?);
                            }
                            buf.advance(body_pos);
                            let body_len: usize = headers.get(header::CONTENT_LENGTH)
                                .ok_or_else(|| format_err!("Missing part Content-Length"))?
                                .to_str()?
                                .parse()?;
                            if body_len > MAX_BODY_LEN {
                                bail!("body length {} exceeds maximum of {}", body_len, MAX_BODY_LEN);
                            }
                            *self = State::Body {
                                headers,
                                body_len,
                            };
                        },
                        httparse::Status::Partial => {
                            if buf.len() > MAX_HEADER_LEN {
                                bail!("header length exceeds maximum of {}", MAX_HEADER_LEN);
                            }
                            return Ok(Poll::Pending)
                        },
                    }
                },
                State::Body { headers, body_len } => {
                    if buf.len() >= *body_len {
                        let body = buf.split_to(*body_len).freeze();
                        let headers = std::mem::replace(headers, HeaderMap::new());
                        *self = State::Boundary { pos: 0 };
                        return Ok(Poll::Ready(Some(Part { headers, body })));
                    }
                    return Ok(Poll::Pending);
                },
                State::Done => return Ok(Poll::Ready(None)),
            }
        }
    }
}

/// Returns a stream of `Part`s in the given HTTP response.
///
/// # Arguments
///
/// * `expected_subtype` should be the expected multipart subtype: `mixed` or `x-mixed-replace`.
/// * `separator` is the newlines (if any) to expect between parts.
//    TODO: it'd be better if this just worked with any number of newlines, as my Javascript
//    parser does.
pub fn parts(r: reqwest::Response, expected_subtype: &str, separator: &'static [u8])
    -> Result<impl Stream<Item = Result<Part, Error>>, Error> {
    if r.status() != reqwest::StatusCode::OK {
        bail!("non-okay status: {:?}", r.status());
    }
    let content_type: mime::Mime = r.headers().get(header::CONTENT_TYPE)
        .ok_or_else(|| format_err!("no content type header"))?
        .to_str()?
        .parse()?;
    parts_inner(content_type, r.bytes_stream(), expected_subtype, separator)
}

fn parts_inner(
    content_type: mime::Mime,
    input: impl Stream<Item = Result<Bytes, reqwest::Error>>,
    expected_subtype: &str,
    separator: &'static [u8])
-> Result<impl Stream<Item = Result<Part, Error>>, Error> {
    // Examine the headers: verify Content-Type is as expected, and determine the boundary.
    let boundary = {
        if content_type.type_() != mime::MULTIPART || content_type.subtype() != expected_subtype {
            bail!("unknown content type {:?}", content_type);
        }
        let boundary = content_type.get_param(mime::BOUNDARY)
            .ok_or_else(|| format_err!("no boundary in mime {:?}", content_type))?;
        let mut line = Vec::with_capacity(separator.len() + boundary.as_str().len() + 4);
        line.extend_from_slice(separator);
        line.extend_from_slice(b"--");
        line.extend_from_slice(boundary.as_str().as_bytes());
        line.extend_from_slice(b"\r\n");
        line
    };

    Ok(Parts {
        input: input,
        buf: BytesMut::new(),
        boundary,
        state: State::Boundary { pos: separator.len() },
    })
}

impl<S> Stream for Parts<S> where S: Stream<Item = Result<Bytes, reqwest::Error>> {
    type Item = Result<Part, Error>;

    fn poll_next(self: Pin<&mut Self>, ctx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        loop {
            match this.state.process(&this.boundary, this.buf) {
                Err(e) => {
                    *this.state = State::Done;
                    return Poll::Ready(Some(Err(e.into())))
                },
                Ok(Poll::Ready(Some(r))) => return Poll::Ready(Some(Ok(r))),
                Ok(Poll::Ready(None)) => return Poll::Ready(None),
                Ok(Poll::Pending) => {},
            }
            match this.input.as_mut().poll_next(ctx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None), // TODO: error on what's in progress.
                Poll::Ready(Some(Err(e))) => {
                    *this.state = State::Done;
                    return Poll::Ready(Some(Err(e.into())))
                },
                Poll::Ready(Some(Ok(b))) => {
                    this.buf.extend_from_slice(&b);
                },
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use failure::Error;
    use futures::stream::StreamExt;
    use super::{Part, parts_inner};

    /// Tries parsing `input` with a stream that has chunks of different sizes arriving.
    /// This ensures that the "not enough data for the current state", "enough for the current state
    /// exactly", "enough for the current state and some for the next", and "enough for the next
    /// state (and beyond)" cases are exercised.
    async fn tester<F>(content_type: mime::Mime, input: &'static [u8], subtype: &'static str,
                    separator: &'static [u8], verify_parts: F)
    where F: Fn(Vec<Result<Part, Error>>) {
        for chunk_size in &[1, 2, usize::MAX] {
            let input: Vec<Result<Bytes, reqwest::Error>> = input
                .chunks(*chunk_size)
                .map(|c: &[u8]| Ok(Bytes::from(c)))
                .collect();
            let input = futures::stream::iter(input);
            let parts = parts_inner(content_type.clone(), input, subtype, separator).unwrap();
            let output_stream: Vec<Result<Part, Error>> = parts.collect().await;
            verify_parts(output_stream);
        }
    }

    #[tokio::test]
    async fn hikvision_style() {
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


        let verify_parts = |parts: Vec<Result<Part, Error>>| {
            let mut i = 0;
            for p in parts {
                let p = p.unwrap();
                assert_eq!(p.headers.get(reqwest::header::CONTENT_TYPE).unwrap().to_str().unwrap(),
                        "application/xml; charset=\"UTF-8\"");
                assert!(p.body.starts_with(b"<EventNotificationAlert"));
                assert!(p.body.ends_with(b"</EventNotificationAlert>\r\n"));
                i += 1;
            }
            assert_eq!(i, 2);
        };
        tester("multipart/mixed; boundary=boundary".parse().unwrap(), input.as_bytes(), "mixed",
               b"", verify_parts).await;
    }

    #[tokio::test]
    async fn dahua_style() {
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
        let verify_parts = |parts: Vec<Result<Part, Error>>| {
            let mut i = 0;
            for p in parts {
                let p = p.unwrap();
                assert_eq!(p.headers.get(reqwest::header::CONTENT_TYPE).unwrap().to_str().unwrap(),
                        "text/plain");
                match i {
                    0 => assert!(p.body.starts_with(b"Code=TimeChange")),
                    1 => assert!(p.body.starts_with(b"Code=NTPAdjustTime")),
                    _ => unreachable!(),
                }
                i += 1;
            }
            assert_eq!(i, 2);
        };
        tester("multipart/x-mixed-replace; boundary=myboundary".parse().unwrap(), input.as_bytes(),
               "x-mixed-replace", b"\r\n\r\n", verify_parts).await;
    }
}
