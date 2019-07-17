use failure::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TopLevel {
    pub time_zone_name: String,
    pub cameras: Vec<Camera>,
    pub session: Option<Session>,
    pub signals: Vec<Signal>,
    pub signal_types: Vec<SignalType>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Session {
    pub username: String,
    pub csrf: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Camera {
    pub uuid: Uuid,
    pub short_name: String,
    pub description: String,
    pub config: Option<CameraConfig>,
    pub streams: BTreeMap<String, Stream>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct CameraConfig {
    pub onvif_host: Option<String>,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Stream {
    pub retain_bytes: i64,
    pub min_start_time_90k: Option<i64>,
    pub max_end_time_90k: Option<i64>,
    pub total_duration_90k: i64,
    pub total_sample_file_bytes: i64,

    pub days: Option<BTreeMap<String, StreamDayValue>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct StreamDayValue {
    pub start_time_90k: i64,
    pub end_time_90k: i64,
    pub total_duration_90k: i64,
}


#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Signal {
    pub id: u32,
    pub cameras: BTreeMap<Uuid, String>,
    pub source: Uuid,
    pub type_: Uuid,
    pub short_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SignalType {
    pub uuid: Uuid,
    pub states: Vec<SignalTypeState>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SignalTypeState {
    pub value: u16,
    pub name: String,

    #[serde(default)]
    pub motion: bool,
    pub color: String,
}

#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub enum PostSignalsEndBase {
    // Epoch,  // unused for now.
    Now,
}

#[derive(Serialize)]
#[serde(rename_all="camelCase")]
pub struct PostSignalsRequest<'a> {
    pub signal_ids: &'a [u32],
    pub states: &'a [u16],

    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time_90k: Option<i64>,

    pub end_base: PostSignalsEndBase,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel_end_time_90k: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct PostSignalsResponse {
    pub time_90k: i64,
}

pub struct Client {
    client: reqwest::Client,
    base_url: reqwest::Url,
    cookie: Option<http::header::HeaderValue>,
}

pub struct TopLevelRequest {
    pub days: bool,
    pub camera_configs: bool,
}

impl Client {
    pub fn new(base_url: reqwest::Url, cookie: Option<reqwest::header::HeaderValue>) -> Self {
        Client {
            client: reqwest::Client::new(),
            base_url,
            cookie,
        }
    }

    pub fn top_level(&self, r: &TopLevelRequest) -> Result<TopLevel, Error> {
        let mut req = self.client.get(self.base_url.join("/api/").unwrap());
        if let Some(c) = self.cookie.as_ref() {
            req = req.header(http::header::COOKIE, c.clone());
        }
        if r.days {
            req = req.query(&[("days", "true")]);
        }
        if r.camera_configs {
            req = req.query(&[("cameraConfigs", "true")]);
        }
        Ok(req.send()?
              .error_for_status()?
              .json()?)
    }

    pub fn update_signals(&self, signal_ids: &[u32], states: &[u16]) -> Result<i64, Error> {
        debug!("update_signals: {:?} -> {:?}", signal_ids, states);
        let body = &PostSignalsRequest {
            signal_ids,
            states,
            start_time_90k: None,
            end_base: PostSignalsEndBase::Now,
            rel_end_time_90k: Some(30 * 90000),
        };
        let mut req = self.client.post(self.base_url.join("/api/signals").unwrap());
        if let Some(c) = self.cookie.as_ref() {
            req = req.header(http::header::COOKIE, c.clone());
        }
        let resp: PostSignalsResponse = req
            .json(&body)
            .send()?
            .error_for_status()?
            .json()?;
        Ok(resp.time_90k)
    }
}
