use chrono::{DateTime, FixedOffset};
use crate::xml;
use failure::{bail, format_err, Error};
use url::Url;

const NS_SOAP: &'static str = "http://www.w3.org/2003/05/soap-envelope";
const NS_DEVICE: &'static str = "http://www.onvif.org/ver10/device/wsdl";
const NS_EVENTS: &'static str = "http://www.onvif.org/ver10/events/wsdl";
const NS_ONVIF: &'static str = "http://www.onvif.org/ver10/schema";
const NS_ADDRESSING: &'static str = "http://www.w3.org/2005/08/addressing";
const NS_NOTIFICATION: &'static str = "http://docs.oasis-open.org/wsn/b-2";

#[derive(Debug)]
pub struct GetCapabilitiesResponse {
    pub events_url: Url,
    pub media_url: Url,
}

impl GetCapabilitiesResponse {
    pub fn parse(doc_str: &str) -> Result<Self, Error> {
        let doc = roxmltree::Document::parse(&doc_str)?;
        let root = doc.root_element();
        if !root.is_element() || root.tag_name() != (NS_SOAP, "Envelope").into() {
            bail!("unexpected root element {:?}", root.tag_name());
        }
        let cap = xml::find(
            doc.root(),
            &[
                (NS_SOAP, "Envelope"),
                (NS_SOAP, "Body"),
                (NS_DEVICE, "GetCapabilitiesResponse"),
                (NS_DEVICE, "Capabilities"),
            ],
        )
        .ok_or_else(|| format_err!("can't find Env/Body/GetCapResp/Cap"))?;
        let events_url = xml::find(
            cap,
            &[
                (NS_ONVIF, "Events"),
                (NS_ONVIF, "XAddr"),
            ],
        )
        .ok_or_else(|| format_err!("can't find Events/XAddr"))?
        .text()
        .ok_or_else(|| format_err!("Events/XAddr has no text"))?;
        let media_url = xml::find(
            cap,
            &[
                (NS_ONVIF, "Media"),
                (NS_ONVIF, "XAddr"),
            ],
        )
        .ok_or_else(|| format_err!("can't find Media/XAddr"))?
        .text()
        .ok_or_else(|| format_err!("Media/XAddr has no text"))?;
        Ok(Self {
            events_url: Url::parse(events_url)?,
            media_url: Url::parse(media_url)?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CreatePullPointSubscriptionResponse {
    pub ref_url: Url,
    pub current_time: DateTime<FixedOffset>,
    pub termination_time: DateTime<FixedOffset>,
}

impl CreatePullPointSubscriptionResponse {
    pub fn parse(doc_str: &str) -> Result<Self, Error> {
        let doc = roxmltree::Document::parse(&doc_str)?;
        let resp = xml::find(
            doc.root(),
            &[
                (NS_SOAP, "Envelope"),
                (NS_SOAP, "Body"),
                (NS_EVENTS, "CreatePullPointSubscriptionResponse"),
            ],
        )
        .ok_or_else(|| format_err!("can't find Env/Body/CreatePullPointSubResp"))?;
        let current_time = xml::find(resp, &[(NS_NOTIFICATION, "CurrentTime")])
            .ok_or_else(|| format_err!("can't find CurrentTime"))?
            .text().ok_or_else(|| format_err!("CurrentTime empty"))?;
        let termination_time = xml::find(resp, &[(NS_NOTIFICATION, "TerminationTime")])
            .ok_or_else(|| format_err!("can't find TerminationTime"))?
            .text().ok_or_else(|| format_err!("TerminationTime empty"))?;
        let addr = xml::find(
            resp,
            &[
                (NS_EVENTS, "SubscriptionReference"),
                (NS_ADDRESSING, "Address"),
            ],
        )
        .ok_or_else(|| format_err!("can't find SubResp/Addr"))?
        .text()
        .ok_or_else(|| format_err!("Address empty"))?;
        Ok(Self {
            ref_url: Url::parse(addr)?,
            current_time: DateTime::parse_from_rfc3339(current_time)?,
            termination_time: DateTime::parse_from_rfc3339(termination_time)?,
        })
    }
}

/// See ONVIF core specification, section 9.5.2.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PropertyOperation {
    Initialized,
    Deleted,
    Changed,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Message {
    /// `{http://www.onvif.org/ver10/topics}RuleEngine/CellMotionDetector/Motion`
    Motion { is_motion: bool },

    /// A message type which hasn't been implemented.
    /// The String is the expanded name of the type, as in `{namespace}local`.
    Other(String),
}

#[derive(Debug, Eq, PartialEq)]
pub struct Notification {
    pub time: DateTime<FixedOffset>,
    pub op: PropertyOperation,
    pub msg: Message,
}

impl Notification {
    pub fn parse_node<'a, 'd>(n: roxmltree::Node<'a, 'd>) -> Result<Self, Error> {
        let topic = {
            let t = xml::find(n, &[(NS_NOTIFICATION, "Topic")])
                .ok_or_else(|| format_err!("no Topic"))?
                .text()
                .ok_or_else(|| format_err!("empty Topic"))?;
            let (prefix, suffix) = match t.find(':') {
                None => (None, t),
                Some(colon) => (Some(&t[0..colon]), &t[colon + 1..]),
            };
            format!(
                "{{{}}}{}",
                n.lookup_namespace_uri(prefix).unwrap_or(""),
                suffix
            )
        };
        let msg = xml::find(n, &[(NS_NOTIFICATION, "Message"), (NS_ONVIF, "Message")])
            .ok_or_else(|| format_err!("no Message"))?;
        let time = DateTime::parse_from_rfc3339(msg
            .attribute("UtcTime")
            .ok_or_else(|| format_err!("no UtcTime on Message"))?)?;
        let op = msg
            .attribute("PropertyOperation")
            .ok_or_else(|| format_err!("no PropertyOperation on Message"))?;
        let op = match op {
            "Initialized" => PropertyOperation::Initialized,
            "Changed" => PropertyOperation::Changed,
            "Deleted" => PropertyOperation::Deleted,
            _ => bail!("invalid PropertyOperation"),
        };
        let msg = match topic.as_str() {
            "{http://www.onvif.org/ver10/topics}RuleEngine/CellMotionDetector/Motion" => {
                let data =
                    xml::find(msg, &[(NS_ONVIF, "Data")]).ok_or_else(|| format_err!("no Data"))?;
                let mut is_motion = None;
                for c in data.children() {
                    if c.has_tag_name((NS_ONVIF, "SimpleItem"))
                        && c.attribute("Name") == Some("IsMotion")
                    {
                        is_motion = Some(match c.attribute("Value") {
                            Some("true") => true,
                            Some("false") => false,
                            _ => bail!("bad IsMotion"),
                        });
                        break;
                    }
                }
                let is_motion = is_motion.ok_or_else(|| format_err!("no IsMotion"))?;
                Message::Motion { is_motion }
            }
            _ => Message::Other(topic),
        };
        Ok(Self { time, op, msg })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct PullMessagesResponse {
    pub current_time: DateTime<FixedOffset>,
    pub termination_time: DateTime<FixedOffset>,
    pub notifications: Vec<Notification>,
}

impl PullMessagesResponse {
    pub fn parse(doc_str: &str) -> Result<Self, Error> {
        let doc = roxmltree::Document::parse(&doc_str)?;
        let resp = xml::find(
            doc.root(),
            &[
                (NS_SOAP, "Envelope"),
                (NS_SOAP, "Body"),
                (NS_EVENTS, "PullMessagesResponse"),
            ],
        )
        .ok_or_else(|| format_err!("can't find Envelope/Body/PullMessagesResponse"))?;
        let mut current_time = None;
        let mut termination_time = None;
        let mut notifications = Vec::new();
        for child in resp.children() {
            if child.has_tag_name((NS_EVENTS, "CurrentTime")) {
                current_time = Some(
                    DateTime::parse_from_rfc3339(child
                        .text()
                        .ok_or_else(|| format_err!("empty CurrentTime"))?)?,
                );
            } else if child.has_tag_name((NS_EVENTS, "TerminationTime")) {
                termination_time = Some(
                    DateTime::parse_from_rfc3339(child
                        .text()
                        .ok_or_else(|| format_err!("empty TerminationTime"))?)?,
                );
            } else if child.has_tag_name((NS_NOTIFICATION, "NotificationMessage")) {
                notifications.push(Notification::parse_node(child)?);
            }
        }
        Ok(Self {
            current_time: current_time
                .ok_or_else(|| format_err!("no CurrentTime"))?
                .to_owned(),
            termination_time: termination_time
                .ok_or_else(|| format_err!("no TerminationTime"))?
                .to_owned(),
            notifications,
        })
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct UnsubscribeResponse {}

impl UnsubscribeResponse {
    pub fn parse(doc_str: &str) -> Result<Self, Error> {
        let doc = roxmltree::Document::parse(&doc_str)?;
        xml::find(
            doc.root(),
            &[
                (NS_SOAP, "Envelope"),
                (NS_SOAP, "Body"),
                (NS_NOTIFICATION, "UnsubscribeResponse"),
            ],
        )
        .ok_or_else(|| format_err!("can't find Envelope/Body/UnsubscribeResponse"))?;
        Ok(Self {})
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, offset::TimeZone};
    use url::Url;

    #[test]
    fn get_capabilities_response() {
        let body = include_str!("testdata/get_capabilities_response.xml");
        let resp = super::GetCapabilitiesResponse::parse(body).unwrap();
        assert_eq!(resp.events_url, Url::parse("http://192.168.5.108/onvif/event_service").unwrap());
        assert_eq!(resp.media_url, Url::parse("http://192.168.5.108/onvif/media_service").unwrap());
    }

    #[test]
    fn create_pull_point_subscription_response() {
        let body = include_str!("testdata/create_pull_point_subscription_response.xml");
        let resp = super::CreatePullPointSubscriptionResponse::parse(body).unwrap();
        assert_eq!(&resp, &super::CreatePullPointSubscriptionResponse {
            current_time: chrono::FixedOffset::east(0).ymd(2019, 2, 10).and_hms(8, 46, 20),
            termination_time: chrono::FixedOffset::east(0).ymd(2019, 2, 10).and_hms(8, 47, 20),
            ref_url: Url::parse("http://192.168.5.108/onvif/Subscription?Idx=11").unwrap(),
        });
    }

    #[test]
    fn pull_messages_response() {
        let body = include_str!("testdata/pull_messages_response.xml");
        let resp = super::PullMessagesResponse::parse(body).unwrap();
        assert_eq!(&resp, &super::PullMessagesResponse {
            current_time: chrono::FixedOffset::east(0).ymd(2019, 2, 11).and_hms(14, 16, 1),
            termination_time: chrono::FixedOffset::east(0).ymd(2019, 2, 11).and_hms(14, 16, 42),
            notifications: vec![
                super::Notification {
                    time: DateTime::parse_from_rfc3339("2019-02-11T14:16:02Z").unwrap(),
                    op: super::PropertyOperation::Initialized,
                    msg: super::Message::Motion {
                        is_motion: false,
                    },
                },
                super::Notification {
                    time: DateTime::parse_from_rfc3339("2019-02-11T14:16:02Z").unwrap(),
                    op: super::PropertyOperation::Initialized,
                    msg: super::Message::Other("{http://www.onvif.org/ver10/topics}Monitoring/ProcessorUsage".to_owned()),
                },
                super::Notification {
                    time: DateTime::parse_from_rfc3339("2019-02-11T14:16:02Z").unwrap(),
                    op: super::PropertyOperation::Initialized,
                    msg: super::Message::Other("{http://www.onvif.org/ver10/topics}Monitoring/OperatingTime/LastReboot".to_owned()),
                },
                super::Notification {
                    time: DateTime::parse_from_rfc3339("2019-02-11T14:16:02Z").unwrap(),
                    op: super::PropertyOperation::Initialized,
                    msg: super::Message::Other("{http://www.onvif.org/ver10/topics}Monitoring/OperatingTime/LastReset".to_owned()),
                },
                super::Notification {
                    time: DateTime::parse_from_rfc3339("2019-02-11T14:16:02Z").unwrap(),
                    op: super::PropertyOperation::Initialized,
                    msg: super::Message::Other("{http://www.onvif.org/ver10/topics}Monitoring/OperatingTime/LastClockSynchronization".to_owned()),
                },
            ],
        });
    }
}
