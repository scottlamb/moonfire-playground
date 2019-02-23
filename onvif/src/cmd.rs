use crate::resp;
use crate::xml;
use failure::Error;
use std::fmt::Write;
use url::Url;

const GET_DEVICE_CAPABILITIES: &'static str = r#"
    <device:GetCapabilities>
      <device:Category>All</device:Category>
    </device:GetCapabilities>
"#;

const CREATE_SUBSCRIPTION: &'static str = r#"
    <events:CreatePullPointSubscription>
      <events:InitialTerminationTime>PT30S</events:InitialTerminationTime>
    </events:CreatePullPointSubscription>
"#;

const PULL_MESSAGES: &'static str = r#"
    <events:PullMessages>
      <events:Timeout>PT10S</events:Timeout>
      <events:MessageLimit>16</events:MessageLimit>
    </events:PullMessages>
"#;

const UNSUBSCRIBE: &'static str = r#"
    <notification:Unsubscribe />
"#;

const GET_METADATA_CONFIGURATIONS: &'static str = r#"
    <media:GetMetadataConfigurations />
"#;

const SET_METADATA_CONFIGURATION_DAHUA: &'static str = r#"
    <media:SetMetadataConfiguration>
      <media:Configuration token="000">
        <onvif:PTZStatus>
          <onvif:Status>false</onvif:Status>
          <onvif:Position>false</onvif:Position>
        </onvif:PTZStatus>
        <onvif:Analytics>true</onvif:Analytics>
        <onvif:Multicast>
          <onvif:Address>
            <onvif:Type>IPv4</onvif:Type>
            <onvif:IPv4Address>224.2.0.0</onvif:IPv4Address>
          </onvif:Address>
          <onvif:Port>40020</onvif:Port>
          <onvif:TTL>64</onvif:TTL>
          <onvif:AutoStart>false</onvif:AutoStart>
        </onvif:Multicast>
        <onvif:SessionTimeout>PT1M</onvif:SessionTimeout>
      </media:Configuration>
      <media:ForcePersistence>true</media:ForcePersistence>
    </media:SetMetadataConfiguration>
"#;

const SET_METADATA_CONFIGURATION_HIKVISION: &'static str = r#"
    <media:SetMetadataConfiguration>
      <media:Configuration token="MetaDataToken">
        <onvif:Name>metaData</onvif:Name>
        <onvif:Events />
        <onvif:PTZStatus>
          <onvif:Status>false</onvif:Status>
          <onvif:Position>false</onvif:Position>
        </onvif:PTZStatus>
        <onvif:Analytics>false</onvif:Analytics>
        <onvif:Multicast>
          <onvif:Address>
            <onvif:Type>IPv4</onvif:Type>
            <onvif:IPv4Address>0.0.0.0</onvif:IPv4Address>
          </onvif:Address>
          <onvif:Port>8600</onvif:Port>
          <onvif:TTL>1</onvif:TTL>
          <onvif:AutoStart>false</onvif:AutoStart>
        </onvif:Multicast>
        <onvif:SessionTimeout>PT5S</onvif:SessionTimeout>
        <onvif:AnalyticsEngineConfiguration/>
      </media:Configuration>
      <media:ForcePersistence>true</media:ForcePersistence>
    </media:SetMetadataConfiguration>
"#;

fn base64_to(from: &[u8], to: &mut [u8]) {
    let len = base64::encode_config_slice(from, base64::STANDARD, to);
    assert_eq!(to.len(), len);
}

pub struct UsernameToken<'a> {
    username: &'a str,
    password: &'a str,
    nonce: [u8; 24],
    created: String,
}

impl<'a> UsernameToken<'a> {
    pub fn new(username: &'a str, password: &'a str) -> Self {
        let mut nonce = [0u8; 24];
        openssl::rand::rand_bytes(&mut nonce).unwrap();
        Self {
            username,
            password,
            nonce,
            created: time::now_utc().rfc3339().to_string(),
        }
    }

    fn nonce_base64(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        base64_to(&self.nonce[..], &mut out);
        out
    }

    fn digest_base64(&self) -> [u8; 28] {
        let mut hasher = openssl::hash::Hasher::new(openssl::hash::MessageDigest::sha1()).unwrap();
        hasher.update(&self.nonce[..]).unwrap();
        hasher.update(self.created.as_bytes()).unwrap();
        hasher.update(self.password.as_bytes()).unwrap();
        let mut out = [0u8; 28];
        base64_to(&hasher.finish().unwrap(), &mut out);
        out
    }
}

fn write_header(w: &mut Write, t: &UsernameToken) -> Result<(), Error> {
    write!(w, r#"<?xml version="1.0" encoding="UTF-8"?>
<s:Envelope xmlns:wsse="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-secext-1.0.xsd"
            xmlns:wsu="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-wssecurity-utility-1.0.xsd"
            xmlns:device="http://www.onvif.org/ver10/device/wsdl"
            xmlns:events="http://www.onvif.org/ver10/events/wsdl"
            xmlns:media="ttp://www.onvif.org/ver10/media/wsdl"
            xmlns:notification="http://docs.oasis-open.org/wsn/b-2"
            xmlns:onvif="http://www.onvif.org/ver10/schema"
            xmlns:s="http://www.w3.org/2003/05/soap-envelope">
  <s:Header>
    <wsse:Security mustUnderstand="true">
      <wsse:UsernameToken>
        <wsse:Username>{}</wsse:Username>
        <wsse:Password Type="http://docs.oasis-open.org/wss/2004/01/oasis-200401-wss-username-token-profile-1.0#PasswordDigest">{}</wsse:Password>
        <wsse:Nonce>{}</wsse:Nonce>
        <wsu:Created>{}</wsu:Created>
      </wsse:UsernameToken>
    </wsse:Security>
  </s:Header>
  <s:Body>"#, xml::EscapedText(t.username), std::str::from_utf8(&t.digest_base64()[..]).unwrap(),
           std::str::from_utf8(&t.nonce_base64()[..]).unwrap(), xml::EscapedText(&t.created))?;
    Ok(())
}

fn write_footer(w: &mut Write) -> Result<(), Error> {
    write!(w, "  </s:Body>\n</s:Envelope>\n")?;
    Ok(())
}

fn get_capabilities_request(t: &UsernameToken) -> Result<String, Error> {
    let mut s = String::new();
    write_header(&mut s, t)?;
    s.push_str(GET_DEVICE_CAPABILITIES);
    write_footer(&mut s)?;
    Ok(s)
}

pub fn get_capabilities(
    cli: &reqwest::Client,
    device_url: Url,
    t: &UsernameToken,
) -> Result<resp::GetCapabilitiesResponse, Error> {
    let body = get_capabilities_request(t)?;
    let mut resp = cli
        .post(device_url)
        .header("Content-Type", "application/soap+xml")
        .header(
            "Soapaction",
            "\"http://www.onvif.org/ver10/device/wsdl/GetCapabilities\"",
        )
        .body(body)
        .send()?
        .error_for_status()?;
    let resp_text = resp.text()?;
    let resp = resp::GetCapabilitiesResponse::parse(&resp_text)?;
    Ok(resp)
}

fn create_pull_point_subscription_request(t: &UsernameToken) -> Result<String, Error> {
    let mut s = String::new();
    write_header(&mut s, t)?;
    s.push_str(CREATE_SUBSCRIPTION);
    write_footer(&mut s)?;
    Ok(s)
}

pub fn create_pull_point_subscription(
    cli: &reqwest::Client,
    events_url: Url,
    t: &UsernameToken,
) -> Result<resp::CreatePullPointSubscriptionResponse, Error> {
    let body = create_pull_point_subscription_request(t)?;
    let mut resp = cli.post(events_url)
        .header("Content-Type", "application/soap+xml")
        .header("Soapaction", "\"http://www.onvif.org/ver10/events/wsdl/EventPortType/CreatePullPointSubscriptionRequest\"")
        .body(body)
        .send()?
        .error_for_status()?;
    let resp_text = resp.text()?;
    let resp = resp::CreatePullPointSubscriptionResponse::parse(&resp_text)?;
    Ok(resp)
}

fn pull_messages_request(t: &UsernameToken) -> Result<String, Error> {
    let mut s = String::new();
    write_header(&mut s, t)?;
    s.push_str(PULL_MESSAGES);
    write_footer(&mut s)?;
    Ok(s)
}

pub fn pull_messages(cli: &reqwest::Client, ref_url: Url, t: &UsernameToken) -> Result<resp::PullMessagesResponse, Error> {
    let body = pull_messages_request(t)?;
    let mut resp = cli
        .post(ref_url)
        .header("Content-Type", "application/soap+xml")
        .header(
            "Soapaction",
            "\"http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/PullMessagesRequest\"",
        )
        .body(body)
        .send()?
        .error_for_status()?;
    let resp_text = resp.text()?;
    resp::PullMessagesResponse::parse(&resp_text)
}

fn unsubscribe_request(t: &UsernameToken) -> Result<String, Error> {
    let mut s = String::new();
    write_header(&mut s, t)?;
    s.push_str(UNSUBSCRIBE);
    write_footer(&mut s)?;
    Ok(s)
}

pub fn unsubscribe(cli: &reqwest::Client, ref_url: Url, t: &UsernameToken) -> Result<resp::UnsubscribeResponse, Error> {
    let body = unsubscribe_request(t)?;
    let mut resp = cli
        .post(ref_url)
        .header("Content-Type", "application/soap+xml")
        .header(
            "Soapaction",
            "\"http://www.onvif.org/ver10/events/wsdl/PullPointSubscription/Unsubscribe\"",
        )
        .body(body)
        .send()?
        .error_for_status()?;
    let resp_text = resp.text()?;
    resp::UnsubscribeResponse::parse(&resp_text)
}

fn get_metadata_configurations_request(t: &UsernameToken) -> Result<String, Error> {
    let mut s = String::new();
    write_header(&mut s, t)?;
    s.push_str(GET_METADATA_CONFIGURATIONS);
    write_footer(&mut s)?;
    Ok(s)
}

pub fn get_metadata_configurations(cli: &reqwest::Client, media_url: Url, t: &UsernameToken) -> Result<String, Error> {
    let body = get_metadata_configurations_request(t)?;
    let mut resp = cli
        .post(media_url)
        .header("Content-Type", "application/soap+xml")
        .header(
            "Soapaction",
            "\"http://www.onvif.org/ver10/media/wsdl/GetMetadataConfigurations\"",
        )
        .body(body)
        .send()?
        .error_for_status()?;
    Ok(resp.text()?)
}

fn set_metadata_configuration_request(t: &UsernameToken) -> Result<String, Error> {
    let mut s = String::new();
    write_header(&mut s, t)?;
    s.push_str(SET_METADATA_CONFIGURATION_DAHUA);
    write_footer(&mut s)?;
    Ok(s)
}

// TODO: response proto.
pub fn set_metadata_configuration(cli: &reqwest::Client, media_url: Url, t: &UsernameToken) -> Result<String, Error> {
    let body = set_metadata_configuration_request(t)?;
    let mut resp = cli
        .post(media_url)
        .header("Content-Type", "application/soap+xml")
        .header(
            "Soapaction",
            "\"http://www.onvif.org/ver10/media/wsdl/SetMetadataConfiguration\"",
        )
        .body(body)
        .send()?
        .error_for_status()?;
    let resp_text = resp.text()?;
    Ok(resp_text)
}

#[cfg(test)]
mod tests {
    use lazy_static::lazy_static;

    lazy_static! {
        static ref TEST_TOKEN: super::UsernameToken<'static> = super::UsernameToken {
            username: "hello",
            password: "world",
            nonce: [
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
                23,
            ],
            created: "2019-02-10T00:00:00Z".to_owned(),
        };
    }

    #[test]
    fn digest() {
        assert_eq!(
            &b"AAECAwQFBgcICQoLDA0ODxAREhMUFRYX"[..],
            &TEST_TOKEN.nonce_base64()[..]
        );
        assert_eq!(
            &b"MFbqn7mNm6RrFtM07jI8yTiAIfg="[..],
            &TEST_TOKEN.digest_base64()[..]
        );
    }

    #[test]
    fn get_capabilities_request() {
        let req_str = super::get_capabilities_request(&TEST_TOKEN).unwrap();
        roxmltree::Document::parse(&req_str).unwrap();
    }

    #[test]
    fn create_pull_point_subscription_request() {
        let req_str = super::create_pull_point_subscription_request(&TEST_TOKEN).unwrap();
        roxmltree::Document::parse(&req_str).unwrap();
    }

    #[test]
    fn set_metadata_configuration_request() {
        let req_str = super::set_metadata_configuration_request(&TEST_TOKEN).unwrap();
        roxmltree::Document::parse(&req_str).unwrap();
    }
}
