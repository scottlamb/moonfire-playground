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

//! Dahua on-camera motion detection.

use failure::{bail, format_err, Error};
use http::header::{self, HeaderValue};
use multipart::{Part, foreach_part};
use openssl::hash;
use regex::Regex;
use reqwest::Client;
use reqwest::Url;
use std::time::Duration;

static IO_TIMEOUT: Duration = Duration::from_secs(120);
static ATTACH_URL: &'static str = "/cgi-bin/eventManager.cgi?action=attach&codes=%5BAll%5D";

pub struct Watcher {
    name: String,
    client: Client,
    url: Url,
    user: String,
    passwd: String,
}

impl Watcher {
    pub fn new(name: String, host: &str, user: impl Into<String>, passwd: impl Into<String>) -> Result<Self, Error> {
        let client = Client::builder()
            .timeout(Some(IO_TIMEOUT))
            .build()?;
        Ok(Watcher {
            name,
            client,
            url: Url::parse(&format!("http://{}{}", host, ATTACH_URL))?,
            user: user.into(),
            passwd: passwd.into(),
        })
    }
}

impl super::Watcher for Watcher {
    fn watch_once(&self) -> Result<(), Error> {
        debug!("{}: watch_once call; url: {}", self.name, self.url);
        let mut resp = self.client.get(self.url.clone())
                                  .send()?;
        if resp.status() == http::StatusCode::UNAUTHORIZED {
            let v = {
                let auth = resp.headers().get(header::WWW_AUTHENTICATE)
                    .ok_or_else(|| format_err!("Unauthorized with no WWW-Authenticate"))?;
                let d = DigestAuthentication::parse(&auth)?;
                d.create(DigestParams {
                    method: "GET",
                    uri: ATTACH_URL,
                    username: &self.user,
                    passwd: &self.passwd,
                    cnonce: &random_cnonce(),
                })
            };
            resp = self.client.get(self.url.clone())
                              .header(header::AUTHORIZATION, v)
                              .send()?;
        }

        let mut resp = resp.error_for_status()?;
        foreach_part(&mut resp, "x-mixed-replace", "\r\n\r\n", &mut |p: Part| {
            let m = p.headers.get(header::CONTENT_TYPE)
                .ok_or_else(|| format_err!("Missing part Content-Type"))?;
            if m.as_bytes() != b"text/plain" {
                bail!("Unexpected part Content-Type {:?}", m);
            }
            use pretty_hex::PrettyHex;
            println!("{:?}", p.body.hex_dump());
            Ok(())
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
struct DigestAuthentication<'a> {
    realm: &'a str,
    qop: &'a str,
    nonce: &'a str,
    opaque: &'a str,
}

#[derive(Debug, Eq, PartialEq)]
struct DigestParams<'a> {
    method: &'a str,
    uri: &'a str,
    username: &'a str,
    passwd: &'a str,
    cnonce: &'a str,
}

/// Returns a hex-encoded version of the input.
fn hex(raw: &[u8]) -> String {
    const HEX_CHARS: [u8; 16] = [b'0', b'1', b'2', b'3', b'4', b'5', b'6', b'7',
                                 b'8', b'9', b'a', b'b', b'c', b'd', b'e', b'f'];
    let mut hex = Vec::with_capacity(2 * raw.len());
    for b in raw {
        hex.push(HEX_CHARS[((b & 0xf0) >> 4) as usize]);
        hex.push(HEX_CHARS[( b & 0x0f      ) as usize]);
    }
    unsafe { String::from_utf8_unchecked(hex) }
}

fn h(items: &[&[u8]]) -> String {
    let mut h = hash::Hasher::new(hash::MessageDigest::md5()).unwrap();;
    for i in items {
        h.update(i).unwrap();
    }
    hex(&h.finish().unwrap())
}

fn random_cnonce() -> String {
    let mut raw = [0u8; 16];
    openssl::rand::rand_bytes(&mut raw).unwrap();
    hex(&raw[..])
}

impl<'a> DigestAuthentication<'a> {
    pub fn parse(h: &'a HeaderValue) -> Result<Self, Error> {
        lazy_static! {
            // This of course isn't general, but it works for my camera.
            // For something general, see:
            // https://github.com/hyperium/headers/issues/21
            static ref START_CODE: Regex = Regex::new(
                "^Digest realm=\"([^\"]*)\", qop=\"(auth)\", nonce=\"([^\"]*)\", \
                opaque=\"([^\"]*)\"$").unwrap();
        }

        let h = h.to_str()?;
        let m = START_CODE.captures(h).ok_or_else(|| format_err!("unparseable WWW-Authenticate"))?;
        Ok(Self {
            realm: m.get(1).expect("realm").as_str(),
            qop: m.get(2).expect("qop").as_str(),
            nonce: m.get(3).expect("nonce").as_str(),
            opaque: m.get(4).expect("opaque").as_str(),
        })
    }

    fn create(&self, p: DigestParams) -> HeaderValue {
        let h_a1 = h(&[p.username.as_bytes(), b":", self.realm.as_bytes(), b":", p.passwd.as_bytes()]);
        let h_a2 = h(&[p.method.as_bytes(), b":", p.uri.as_bytes()]);
        let nc = "00000001";
        let response = h(&[h_a1.as_bytes(), b":",
                           self.nonce.as_bytes(), b":", nc.as_bytes(), b":", p.cnonce.as_bytes(),
                           b":", self.qop.as_bytes(), b":", h_a2.as_bytes()]);
        HeaderValue::from_str(&format!(
                "Digest username=\"{}\", realm=\"{}\", uri=\"{}\", algorithm={}, nonce=\"{}\", \
                nc={}, cnonce=\"{}\", qop={}, response=\"{}\", opaque=\"{}\"",
                p.username, self.realm, p.uri, "MD5", self.nonce, nc, p.cnonce, self.qop, response,
                self.opaque)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use http::header::HeaderValue;

    #[test]
    fn parse_www_authenticate() {
        // Example taken from a live camera.
        let v = HeaderValue::from_str(
            "Digest realm=\"Login to 3EPAA7EF4DC8055\", qop=\"auth\", nonce=\"1739884596\", \
            opaque=\"ce65875b0ce375169e3eab8dfa7cd06b3f5d8d4c\"").unwrap();
        let a = super::DigestAuthentication::parse(&v).unwrap();
        assert_eq!(&a, &super::DigestAuthentication {
            realm: "Login to 3EPAA7EF4DC8055",
            qop: "auth",
            nonce: "1739884596",
            opaque: "ce65875b0ce375169e3eab8dfa7cd06b3f5d8d4c",
        });
    }

    #[test]
    fn create_authorization() {
        // Example taken from RFC 7616 section 3.9.1.
        let d = super::DigestAuthentication {
            realm: "http-auth@example.org",
            qop: "auth",
            nonce: "7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v",
            opaque: "FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS",
        };
        let v = d.create(super::DigestParams {
            method: "GET",
            uri: "/dir/index.html",
            username: "Mufasa",
            passwd: "Circle of Life",
            cnonce: "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
        });
        assert_eq!(v.to_str().unwrap(),
                   "Digest username=\"Mufasa\", \
                   realm=\"http-auth@example.org\", \
                   uri=\"/dir/index.html\", \
                   algorithm=MD5, \
                   nonce=\"7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v\", \
                   nc=00000001, \
                   cnonce=\"f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ\", \
                   qop=auth, \
                   response=\"8ca523f5e9506fed4657c9700eebdbec\", \
                   opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\"");
    }
}
