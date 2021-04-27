// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 Scott Lamb <slamb@slamb.org>
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

//! RTSP metadata stream on-camera motion detection.
//! This doesn't actually do anything with the stream yet; it just connects.

use failure::{format_err, Error, ResultExt};
use log::info;
use reqwest::Url;
use serde::Deserialize;
use std::ffi::CString;

#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
pub struct WatcherConfig {
    camera_name: String,
}

struct Watcher {
    name: String,
    url: Url,
}

impl WatcherConfig {
    pub fn start(self, ctx: &crate::Context) -> Result<tokio::task::JoinHandle<()>, Error> {
        let camera = ctx.cameras_by_name.get(self.camera_name.as_str()).ok_or_else(|| format_err!("RTSP camera {}: no such camera in NVR", &self.camera_name))?;
        let nvr_config = camera.config.as_ref().ok_or_else(|| format_err!("RTSP camera {}: no config", &self.camera_name))?;
        let stream_config =
            camera.streams.iter()
            .next().ok_or_else(|| format_err!("camera {} has no streams", &self.camera_name))?
            .1.config.as_ref()
            .ok_or_else(|| format_err!("camera {} has no config for first stream", &self.camera_name))?;
        let mut url = Url::parse(&stream_config.rtsp_url)?;
        info!("{}: url={}", &self.camera_name, &url);
        url.set_username(&nvr_config.username).unwrap();
        url.set_password(Some(&nvr_config.password)).unwrap();
        let mut w = Watcher {
            name: self.camera_name,
            url,
        };
        Ok(tokio::task::spawn_blocking({
            move || {
                loop {
                    if let Err(e) = w.watch_once() {
                        error!("{}: {}", &w.name, moonfire_base::prettify_failure(&e));
                        std::thread::sleep(std::time::Duration::from_secs(55));
                    }
                }
            }
        }))
    }
}

impl Watcher {
    fn watch_once(&mut self) -> Result<(), Error> {
        let mut open_options = moonfire_ffmpeg::avutil::Dictionary::new();
        open_options.set(cstr!("rtsp_transport"), cstr!("tcp")).unwrap();
        open_options.set(cstr!("user-agent"), cstr!("moonfire-camera-analytics")).unwrap();
        // 120-second socket timeout, in microseconds.
        open_options.set(cstr!("stimeout"), cstr!("120000000")).unwrap();
        open_options.set(cstr!("allowed_media_types"), cstr!("data")).unwrap();
        open_options.set(cstr!("fflags"), cstr!("nobuffer")).unwrap();
        info!("{}: opening", &self.name);
        let mut input = moonfire_ffmpeg::avformat::InputFormatContext::open(
            &CString::new(self.url.as_str()).unwrap(), &mut open_options)?;
        if !open_options.empty() {
            warn!("Some options were not understood: {}", open_options);
        }

        input.find_stream_info()?;

        let s = input.streams();
        assert_eq!(s.len(), 1);

        loop {
            let p = input.read_frame()?;
            let bytestring = p.data().unwrap();
            let element = xmltree::Element::parse(bytestring)
                .with_context(|_| format!("unable to parse {:?}", &String::from_utf8_lossy(bytestring)))?;
            let mut pretty = Vec::new();
            let cfg = xmltree::EmitterConfig::new().perform_indent(true);
            element.write_with_config(&mut pretty, cfg)?;
            let wrapped_pretty: bstr::BString = pretty.into();
            info!("{}: packet:\n{}", &self.name, &wrapped_pretty);
        }
    }
}
