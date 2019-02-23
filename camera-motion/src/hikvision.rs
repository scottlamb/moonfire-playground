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

//! Hikvision on-camera motion detection.
//!
//! Hikvision cameras support ONVIF, but ONVIF is tedious to implement. It's based on SOAP, which
//! is an unpleasant protocol which has no pure Rust libraries. Hikvision supports a second,
//! apparently proprietary, protocol which is simpler. It's documented on their website
//! (search for `hikvision isapi filetype:pdf`) but the documentation is incorrect or ambiguous in
//! several ways:
//!
//!    * the URL is incorrect; the actual URL does not have the initial `/ISAPI` path segment.
//!    * the XML namespace may be either of those listed in the `NAMESPACES` constant; neither is
//!      as documented.
//!    * the `&lt;DetectionRegionList>` is never populated.
//!    * when idle, there's a near-constant flow of inactive `videoloss` messages (many per second,
//!      possibly depending on how quickly the client reads them).
//!    * the `VMD` event type (motion events) never sends `&lt;eventState>inactive&lt;/eventState>`
//!      messages. Instead, the inactive `videoloss` messages appear to mark the end of all events.
//!      The `videoloss` messages apparently stop while other events are active.
//!    * the `activePostCount` is non-decreasing for event types, not for single events. It's
//!      apparently useless.

use bytes::{BufMut, BytesMut};
use failure::{bail, format_err, Error};
use http::header::{self, HeaderValue};
use mime;
use multipart::{Part, foreach_part};
use reqwest::Client;
use reqwest::Url;
use std::io::Write;
use std::time::Duration;
use xml;

/// Expected namespace for all XML elements in the response.
static NAMESPACES: [&'static str; 2] = [
    "http://www.hikvision.com/ver10/XMLSchema",  // used by firmware V5.3.0 build 150513
    "http://www.std-cgi.com/ver10/XMLSchema",    // used by firmware V4.0.9 130306
];

/// I/O timeout. The camera sends a few events per second, even when there's nothing to report.
static IO_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Watcher {
    name: String,
    client: Client,
    url: Url,
    auth: HeaderValue,
}

impl Watcher {
    pub fn new(name: String, host: &str, user: &str, password: &str) -> Result<Self, Error> {
        // TODO: is there a separate connect timeout?
        let client = Client::builder()
            .timeout(Some(IO_TIMEOUT))
            .build()?;
        Ok(Watcher {
            name,
            client,
            url: Url::parse(&format!("http://{}/Event/notification/alertStream", host))?,
            auth: basic_auth(user, password),
        })
    }
}

impl super::Watcher for Watcher {
    fn watch_once(&self) -> Result<(), Error> {
        debug!("{}: watch_once call; url: {}", self.name, self.url);
        let mut resp = self.client.get(self.url.clone())
                                  .header(header::AUTHORIZATION, &self.auth)
                                  .send()?
                                  .error_for_status()?;

        let mut vmd_active = false;
        foreach_part(&mut resp, "mixed", "", &mut |p: Part| {
            let m: mime::Mime = p.headers.get(header::CONTENT_TYPE)
                .ok_or_else(|| format_err!("Missing part Content-Type"))?
                .to_str()?
                .parse()?;
            if m.type_() != "application" || m.subtype() != "xml" {
                bail!("Unexpected part Content-Type {}", m);
            }
            let notification = parse(p.body)?;
            let (event_type, active) = match (notification.event_type, notification.active) {
                (Some(t), Some(a)) => (t, a),
                _ => bail!("body {:?} must specify event type and state", p.body),
            };
            trace!("{}: notification: {} active={}", self.name, event_type, active);
            if event_type == "VMD" && active && !vmd_active {
                info!("{}: motion event started", self.name);
                vmd_active = true;
            } else if !active && vmd_active {
                info!("{}: motion event ended", self.name);
                vmd_active = false;
            }
            Ok(())
        })
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct Notification {
    event_type: Option<String>,
    active: Option<bool>,
}

enum NotificationElement {
    Type,
    State,
}

fn parse(body: &[u8]) -> Result<Notification, Error> {
    let mut reader = xml::EventReader::new(body);
    let mut depth = 0;
    let mut n = Notification::default();
    let mut active: Option<NotificationElement> = None;
    loop {
        match reader.next()? {
            xml::reader::XmlEvent::StartElement{name, ..} => {
                trace!("...starting element: {:?}", name);
                depth += 1;
                let in_expected_ns = match name.namespace_ref() {
                    None => false,
                    Some(ref x) => NAMESPACES.contains(x),
                };
                if depth == 1 {
                    if !in_expected_ns || name.local_name != "EventNotificationAlert" {
                        bail!("Unexpected top-level element {:?}", name);
                    }
                } else if depth == 2 {
                    if !in_expected_ns {
                        continue
                    }
                    active = match name.local_name.as_str() {
                        "eventType" => Some(NotificationElement::Type),
                        "eventState" => Some(NotificationElement::State),
                        _ => None,
                    };
                }
            },
            xml::reader::XmlEvent::EndElement{..} => {
                depth -= 1;
                active = None;
            },
            xml::reader::XmlEvent::EndDocument => return Ok(n),
            xml::reader::XmlEvent::Characters(c) => match active {
                Some(NotificationElement::Type) => n.event_type = Some(c),
                Some(NotificationElement::State) => n.active = Some(match c.as_str() {
                    "active" => true,
                    "inactive" => false,
                    _ => bail!("invalid eventState: {}", c),
                }),
                None => {},
            },
            _ => {},
        };
    }
}

fn basic_auth(username: &str, password: &str) -> HeaderValue {
    let mut b = BytesMut::with_capacity("Basic ".len() + (username.len() + password.len() + 1) * 4 / 3 + 4);
    b.extend_from_slice(b"Basic ");
    let mut w = b.writer();
    {
        let mut e = base64::write::EncoderWriter::new(&mut w, base64::STANDARD);
        e.write_all(username.as_bytes()).unwrap();
        e.write_all(b":").unwrap();
        e.write_all(password.as_bytes()).unwrap();
        e.finish().unwrap();
    }
    HeaderValue::from_shared(w.into_inner().freeze()).unwrap()
}

#[cfg(test)]
mod tests {
    use std::sync;
    use super::{Notification, parse};

    static INIT: sync::Once = sync::ONCE_INIT;

    fn init() {
        INIT.call_once(super::super::init_logging);
    }

    #[test]
    fn parse_boring_body() {
        init();
        let body = b"\
            <EventNotificationAlert version=\"1.0\" \
            xmlns=\"http://www.hikvision.com/ver10/XMLSchema\">\n\
            <ipAddress>192.168.5.106</ipAddress>\n\
            <portNo>80</portNo>\n\
            <protocol>HTTP</protocol>\n\
            <macAddress>8c:e7:48:da:94:8f</macAddress>\n\
            <channelID>1</channelID>\n\
            <dateTime>2016-12-24T18:59:49-8:00</dateTime>\n\
            <activePostCount>0</activePostCount>\n\
            <eventType>videoloss</eventType>\n\
            <eventState>inactive</eventState>\n\
            <eventDescription>videoloss alarm</eventDescription>\n\
            </EventNotificationAlert>";
        assert_eq!(parse(body).unwrap(), Notification {
            event_type: Some("videoloss".to_owned()),
            active: Some(false),
        });
    }

    #[test]
    fn parse_interesting_body() {
        init();
        let body = b"\
            <?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
            <EventNotificationAlert version=\"2.0\" \
            xmlns=\"http://www.std-cgi.com/ver10/XMLSchema\">\n\
            <ipAddress>172.6.64.7</ipAddress>\n\
            <portNo>80</portNo>\n\
            <protocol>HTTP</protocol>\n\
            <macAddress>01:17:24:45:D9:F4</macAddress>\n\
            <channelID>1</channelID>\n\
            <dateTime>2009-11-14T15:27Z</dateTime>\n\
            <activePostCount>1</activePostCount>\n\
            <eventType>VMD</eventType>\n\
            <eventState>active</eventState>\n\
            <eventDescription>Motion alarm</eventDescription>\n\
            <DetectionRegionList>\n\
            <DetectionRegionEntry>\n\
            <regionID>2</regionID>\n\
            <sensitivityLevel>4</sensitivityLevel>\n\
            </DetectionRegionEntry>\n\
            </DetectionRegionList>\n\
            </EventNotificationAlert>\n";
        assert_eq!(parse(body).unwrap(), Notification {
            event_type: Some("VMD".to_owned()),
            active: Some(true),
        });
    }

    #[test]
    fn basic_auth() {
        assert_eq!(super::basic_auth("Aladdin", "OpenSesame").to_str().unwrap(),
                   "Basic QWxhZGRpbjpPcGVuU2VzYW1l");
    }
}
