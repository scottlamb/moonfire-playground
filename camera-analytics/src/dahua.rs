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

use crate::send_with_timeout;
use crate::multipart::{Part, self};
use failure::{bail, format_err, Error};
use futures::StreamExt;
use regex::Regex;
use reqwest::header;
use reqwest::Client;
use reqwest::Url;
use std::collections::BTreeMap;
use serde::Deserialize;
use std::convert::TryFrom;
use std::time::Duration;

static CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
static IDLE_TIMEOUT: Duration = Duration::from_secs(15);
static ATTACH_URL: &'static str = "/cgi-bin/eventManager.cgi?action=attach&codes=%5BAll%5D&heartbeat=5";

const UNKNOWN: u16 = 0;
const STILL: u16 = 1;
const MOVING: u16 = 2;

#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WatcherConfig {
    camera_name: String,
    signals: Vec<SignalConfig>,
}

#[derive(Copy, Clone, Deserialize)]
enum MotionType {
    VideoMotion,
    SmartMotionHuman,
    SmartMotionVehicle,
}

impl MotionType {
    fn as_str(self) -> &'static str {
        match self {
            MotionType::VideoMotion => "VideoMotion",
            MotionType::SmartMotionHuman => "SmartMotionHuman",
            MotionType::SmartMotionVehicle => "SmartMotionVehicle",
        }
    }
}

impl Default for MotionType {
    fn default() -> Self {
        MotionType::VideoMotion
    }
}

#[derive(Copy, Clone, Deserialize)]
enum IvsType {
    CrossLineDetection,
    ParkingDetection,
}

impl IvsType {
    fn as_str(self) -> &'static str {
        match self {
            IvsType::CrossLineDetection => "CrossLineDetection",
            IvsType::ParkingDetection => "ParkingDetection",
        }
    }
}

#[derive(Copy, Clone, Deserialize)]
enum ObjectType {
    Human,
    Vehicle,
}

impl ObjectType {
    fn as_str(self) -> &'static str {
        match self {
            ObjectType::Human => "Human",
            ObjectType::Vehicle => "Vehicle",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all="camelCase", tag = "type")]
enum SignalConfig {
    #[serde(rename_all="camelCase")]
    Motion {
        signal_name: String,
        region: Option<String>,

        #[serde(default)]
        motion_type: MotionType,
    },
    #[serde(rename_all="camelCase")]
    Ivs {
        signal_name: String,
        ivs_type: IvsType,
        rule_name: String,
        object_type: Option<ObjectType>,
    },
}

/// Given the payload for a motion event, checks if it has the expected region name.
fn regions_match(m: &serde_json::Map<String, serde_json::Value>, expected: Option<&str>) -> bool {
    match (m.get("RegionName"), expected) {
        (None, None) => true,
        (Some(_), None) => false,
        (None, Some(_)) => false,
        (Some(serde_json::Value::Array(ref a)), Some(expected)) => {
            for r in a {
                match r {
                    serde_json::Value::String(ref s) => {
                        if s == expected {
                            return true;
                        }
                    },
                    _ => {
                        warn!("Non-string region in {:?}", a);
                        continue;
                    }
                }
            }
            false
        },
        (Some(o), _) => {
            warn!("Motion event with non-array RegionName: {:?}", o);
            false
        }
    }
}

impl SignalConfig {
    fn process(&self, e: &Event) -> Option<u16> {
        match self {
            SignalConfig::Motion { region, motion_type, .. } => {
                if e.code.as_str() != motion_type.as_str() {
                    return None;
                }
                let m = match e.data {
                    Some(serde_json::Value::Object(ref m)) => m,
                    Some(_) => {
                        warn!("Motion event of type {} with non-object data: {:?}", &e.code, e.data);
                        return None;
                    }
                    None => {
                        warn!("Motion event of type {} with no data", &e.code);
                        return None;
                    },
                };
                if !regions_match(m, region.as_ref().map(|s| s.as_str())) {
                    return None;
                }
            },
            SignalConfig::Ivs { ivs_type, rule_name, object_type, .. } => {
                if e.code.as_str() != ivs_type.as_str() {
                    return None;
                }
                let m = match e.data {
                    Some(serde_json::Value::Object(ref m)) => m,
                    Some(_) => {
                        warn!("IVS event of type {} with non-object data: {:?}", &e.code, e.data);
                        return None;
                    }
                    None => {
                        warn!("IVS event of type {} with no data", &e.code);
                        return None;
                    },
                };
                match m.get("Name") {
                    None => {
                        warn!("IVS event with no rule name: {:?}", m);
                        return None;
                    },
                    Some(name) => {
                        if name != rule_name {
                            return None;
                        }
                    },
                }
                if let Some(object_type) = object_type {
                    let obj = match m.get("Object") {
                        None => return None,
                        Some(serde_json::Value::Object(ref m)) => m,
                        Some(_) => {
                            warn!("IVS event of type {} with Object that isn't a JSON object: {:?}", &e.code, &e.data);
                            return None;
                        }
                    };
                    let t = match obj.get("ObjectType") {
                        None => return None,
                        Some(serde_json::Value::String(s)) => s.as_str(),
                        Some(_) => {
                            warn!("IVS event of type {} with non-string Object.ObjectType: {:?}", &e.code, &e.data);
                            return None;
                        }
                    };
                    if t != object_type.as_str() {
                        return None;
                    }
                }
            }
        }
        match e.action.as_str() {
            "Start" => return Some(MOVING),
            "Stop" => return Some(STILL),
            _ => {
                warn!("Unknown action {}", &e.action);
                return None;
            }
        }
    }
}

struct Signal {
    id: u32,
    cfg: SignalConfig,
}

pub struct Watcher {
    name: String,
    client: Client,
    dry_run: bool,
    url: Url,
    username: String,
    password: String,
    updater: moonfire_nvr_client::updater::SignalUpdaterSender,
    signals: Vec<Signal>,
}

impl WatcherConfig {
    pub(crate) fn start(self, ctx: &crate::Context) -> Result<tokio::task::JoinHandle<()>, Error> {
        let camera_name = self.camera_name;
        let camera = ctx.cameras_by_name.get(camera_name.as_str()).ok_or_else(|| format_err!("Dahua camera {}: no such camera in NVR", &camera_name))?;
        let nvr_config = camera.config.as_ref().ok_or_else(|| format_err!("Dahua camera {}: no config", &camera_name))?;
        let client = Client::new();
        let onvif_base_url = nvr_config.onvif_base_url.as_ref()
            .ok_or_else(|| format_err!("Dahua camera {} has no ONVIF base URL", &camera_name))?;
        let mut signals = Vec::with_capacity(self.signals.len());
        for cfg in self.signals {
            let n = match &cfg {
                SignalConfig::Motion { signal_name, .. } => signal_name.as_str(),
                SignalConfig::Ivs { signal_name, .. } => signal_name.as_str(),
            };
            let nvr_signal = ctx.signals_by_name.get(n).ok_or_else(|| format_err!("Dahua camera {}: no such signal {}", &camera_name, n))?;
            signals.push(Signal {
                id: nvr_signal.id,
                cfg,
            });
        }
        let mut w = Watcher {
            name: camera_name,
            client,
            dry_run: ctx.dry_run,
            url: onvif_base_url.join(ATTACH_URL)?,
            username: nvr_config.username.clone(),
            password: nvr_config.password.clone(),
            updater: ctx.updater.clone(),
            signals,
        };
        Ok(tokio::spawn(async move {
            w.set_all_unknown();
            loop {
                if let Err(e) = w.watch_once().await {
                    w.set_all_unknown();
                    error!("{}: {:?}", &w.name, e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }))
    }
}

impl Watcher {
    async fn watch_once(&mut self) -> Result<(), Error> {
        debug!("{}: watch_once call; url: {}", self.name, self.url);

        // Open the URL, giving up after CONNECT_TIMEOUT.
        let mut resp = send_with_timeout(CONNECT_TIMEOUT, self.client.get(self.url.clone())).await?;
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            let v = {
                let auth = resp.headers().get_all(header::WWW_AUTHENTICATE);
                let mut client =
                    http_auth::PasswordClient::try_from(auth).map_err(failure::err_msg)?;
                client
                    .respond(&http_auth::PasswordParams {
                        username: &self.username,
                        password: &self.password,
                        uri: &self.url.as_str(),
                        method: "GET",
                        body: Some(&[]),
                    })
                    .map_err(failure::err_msg)?
            };
            resp = send_with_timeout(
                CONNECT_TIMEOUT,
                self.client.get(self.url.clone())
                           .header(header::AUTHORIZATION, v))
                .await?;
        }

        let resp = resp.error_for_status()?;
        let mut parts = Box::pin(multipart::parse(resp, "x-mixed-replace")?);
        loop {
            let p: Part = match tokio::time::timeout(IDLE_TIMEOUT, parts.next()).await {
                Ok(None) => bail!("unexpected end of multipart stream"),
                Ok(Some(p)) => p?,
                Err(_) => bail!("idle timeout"),
            };
            let m = p.headers.get(header::CONTENT_TYPE)
                .ok_or_else(|| format_err!("Missing part Content-Type"))?;
            if m.as_bytes() != b"text/plain" {
                bail!("Unexpected part Content-Type {:?}", m);
            }
            let body = std::str::from_utf8(&p.body)?;
            if body == "Heartbeat" {
                continue;
            }
            let e = Event::parse(body)?;
            if e.code == "VideoMotionInfo" { // spammy
                trace!("{}:\n{}", &self.name, body);
            } else {
                debug!("{}:\n{}", &self.name, body);
            }
            trace!("{}: event: {:#?}", &self.name, &e);
            let mut m = BTreeMap::new();
            for s in &self.signals {
                if let Some(new_state) = s.cfg.process(&e) {
                    m.insert(s.id, new_state);
                }
            }
            if m.is_empty() {
                continue;
            }
            debug!("{}: {:#?}", &self.name, &m);
            if !self.dry_run {
                self.updater.update(m);
            }
        }
    }

    fn set_all_unknown(&self) {
        if self.dry_run {
            return;
        }
        let mut m = BTreeMap::new();
        for s in &self.signals {
            m.insert(s.id, UNKNOWN);
        }
        self.updater.update(m);
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
    password: &'a str,
    cnonce: &'a str,
}

#[derive(Debug, PartialEq)]
pub struct Event {
    pub code: String,
    pub action: String,
    pub index: u32,
    pub data: Option<serde_json::Value>,
}

impl Event {
    pub fn parse(raw: &str) -> Result<Event, Error> {
        lazy_static! {
            static ref EVENT: Regex = Regex::new(
                r"(?s)^Code=([^;]+);action=([^;]+);index=([0-9]+)(?:;data=(\{.*\}))?\s*$").unwrap();
        }
        let m = EVENT.captures(raw).ok_or_else(|| format_err!("unparseable event: {:?}", raw))?;
        Ok(Self {
            code: m.get(1).expect("code").as_str().to_owned(),
            action: m.get(2).expect("action").as_str().to_owned(),
            index: m.get(3).expect("index").as_str().parse()?,
            data: match m.get(4) {
                None => None,
                Some(d) => Some(serde_json::from_str(d.as_str())?),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Event;

    #[test]
    fn parse_time_change() {
        let raw = include_str!("testdata/dahua/timechange-pulse");
        assert_eq!(Event::parse(raw).unwrap(), Event {
            code: "TimeChange".to_owned(),
            action: "Pulse".to_owned(),
            index: 0,
            data: Some(json!({
                "BeforeModifyTime": "2021-04-12 16:39:33",
                "ModifiedTime": "2021-04-12 16:39:33",
            })),
        });
    }

    #[test]
    fn parse_ntp_adjust_time() {
        let raw = include_str!("testdata/dahua/ntpadjusttime-pulse");
        assert_eq!(Event::parse(raw).unwrap(), Event {
            code: "NTPAdjustTime".to_owned(),
            action: "Pulse".to_owned(),
            index: 0,
            data: Some(json!({
                "Address": "192.168.5.254",
                "Before": "2021-04-12 16:39:32",
                "result": true,
            })),
        });
    }

    #[test]
    fn parse_video_motion_info_state() {
        let raw = include_str!("testdata/dahua/videomotioninfo-state");
        assert_eq!(Event::parse(raw).unwrap(), Event {
            code: "VideoMotionInfo".to_owned(),
            action: "State".to_owned(),
            index: 0,
            data: None,
        });
    }

    #[test]
    fn parse_video_motion_start() {
        let raw = include_str!("testdata/dahua/videomotion-start");
        assert_eq!(Event::parse(raw).unwrap(), Event {
            code: "VideoMotion".to_owned(),
            action: "Start".to_owned(),
            index: 0,
            data: Some(json!({
                "Id": [0],
                "RegionName": ["driveway"],
                "SmartMotionEnable": true,
            })),
        });
    }

    #[test]
    fn parse_video_motion_stop() {
        let raw = include_str!("testdata/dahua/videomotion-stop");
        assert_eq!(Event::parse(raw).unwrap(), Event {
            code: "VideoMotion".to_owned(),
            action: "Stop".to_owned(),
            index: 0,
            data: Some(json!({
                "Id": [0],
                "RegionName": ["driveway"],
                "SmartMotionEnable": true,
            })),
        });
    }
}
