use failure::Error;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use log::debug;
use uuid::Uuid;

pub use moonfire_base::time::{Time, Duration};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct TopLevel {
    pub time_zone_name: String,
    pub cameras: Vec<Camera>,
    pub session: Option<Session>,
    pub signals: Vec<Signal>,
    pub signal_types: Vec<SignalType>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Session {
    pub username: String,
    pub csrf: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Camera {
    pub uuid: Uuid,
    pub short_name: String,
    pub description: String,
    pub config: Option<CameraConfig>,
    pub streams: BTreeMap<String, Stream>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct CameraConfig {
    pub onvif_host: Option<String>,
    pub username: String,
    pub password: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Stream {
    pub retain_bytes: i64,
    pub min_start_time_90k: Option<i64>,
    pub max_end_time_90k: Option<i64>,
    pub total_duration_90k: i64,
    pub total_sample_file_bytes: i64,

    pub days: Option<BTreeMap<String, StreamDayValue>>,
    pub config: Option<StreamConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct StreamConfig {
    pub rtsp_url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct StreamDayValue {
    pub start_time_90k: i64,
    pub end_time_90k: i64,
    pub total_duration_90k: i64,
}

pub struct ListRecordingsRequest<'a> {
    pub camera: Uuid,
    pub stream: &'a str,
    pub start: Option<Time>,
    pub end: Option<Time>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct ListRecordings {
    pub recordings: Vec<Recording>,

    #[serde(default)]
    pub video_sample_entries: BTreeMap<i32, VideoSampleEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Recording {
    pub start_time_90k: i64,
    pub end_time_90k: i64,
    pub sample_file_bytes: i64,
    pub video_samples: i64,
    pub video_sample_entry_id: Option<String>,  // mandatory in new versions
    pub start_id: i32,
    pub open_id: u32,
    pub first_uncommitted: Option<i32>,
    pub end_id: Option<i32>,

    #[serde(default)]
    pub growing: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct VideoSampleEntry {
    pub width: u16,
    pub height: u16,
    pub pasp_h_spacing: u16,
    pub pasp_v_spacing: u16,
}

pub enum Mp4Type {
    Normal,
    Fragment,
}

impl std::fmt::Display for Mp4Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Mp4Type::Normal => "mp4",
            Mp4Type::Fragment => "m4s",
        })
    }
}

pub struct ViewRequest<'a> {
    pub camera: Uuid,
    pub mp4_type: Mp4Type,
    pub stream: &'a str,
    pub s: &'a str,
    pub ts: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct Signal {
    pub id: u32,
    pub cameras: BTreeMap<Uuid, String>,
    pub source: Uuid,
    pub type_: Uuid,
    pub short_name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SignalType {
    pub uuid: Uuid,
    pub states: Vec<SignalTypeState>,
}

#[derive(Clone, Debug, Deserialize)]
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct PostSignalsResponse {
    pub time_90k: i64,
}

pub struct Client {
    client: reqwest::Client,
    base_url: reqwest::Url,
    cookie: Option<reqwest::header::HeaderValue>,
}

#[derive(Default)]
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

    pub async fn top_level(&self, r: &TopLevelRequest) -> Result<TopLevel, Error> {
        let mut req = self.client.get(self.base_url.join("/api/")?);
        if let Some(c) = self.cookie.as_ref() {
            req = req.header(reqwest::header::COOKIE, c.clone());
        }
        if r.days {
            req = req.query(&[("days", "true")]);
        }
        if r.camera_configs {
            req = req.query(&[("cameraConfigs", "true")]);
        }
        Ok(req.send().await?
              .error_for_status()?
              .json().await?)
    }

    pub async fn update_signals(&self, signal_ids: &[u32], states: &[u16]) -> Result<i64, Error> {
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
            req = req.header(reqwest::header::COOKIE, c.clone());
        }
        let resp: PostSignalsResponse = req
            .json(&body)
            .send().await?
            .error_for_status()?
            .json().await?;
        Ok(resp.time_90k)
    }

    pub async fn list_recordings(&self, r: &ListRecordingsRequest<'_>)
                                 -> Result<ListRecordings, Error> {
        let mut req = self.client.get(self.base_url.join(&format!("/api/cameras/{}/{}/recordings",
                                                                  r.camera, r.stream))?);
        if let Some(c) = self.cookie.as_ref() {
            req = req.header(reqwest::header::COOKIE, c.clone());
        }
        if let Some(s) = r.start {
            req = req.query(&[("startTime90k", &s.0.to_string())]);
        }
        if let Some(e) = r.end {
            req = req.query(&[("endTime90k", &e.0.to_string())]);
        }
        Ok(req.send().await?
              .error_for_status()?
              .json().await?)
    }

    pub async fn view(&self, r: &ViewRequest<'_>) -> Result<reqwest::Response, Error> {
        let mut req = self.client
            .get(self.base_url.join(&format!("/api/cameras/{}/{}/view.{}",
                                             r.camera, r.stream, r.mp4_type))?)
            .query(&[
                ("s", r.s),
                ("ts", if r.ts { "true" } else { "false" }),
            ]);
        if let Some(c) = self.cookie.as_ref() {
            req = req.header(reqwest::header::COOKIE, c.clone());
        }
        Ok(req.send().await?.error_for_status()?)
    }
}
