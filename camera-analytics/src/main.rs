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

#[macro_use] extern crate cstr;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
#[cfg_attr(test, macro_use)] extern crate serde_json;

mod dahua;
mod hikvision;
mod multipart;
mod rtsp;

use failure::{Error, format_err};
use futures::TryFutureExt;
use fnv::FnvHashMap;
use std::future::Future;
use std::str::FromStr;
use structopt::StructOpt;
use uuid::Uuid;

#[derive(StructOpt)]
struct Opt {
    #[structopt(short, long, parse(try_from_str))]
    cookie: Option<reqwest::header::HeaderValue>,

    #[structopt(short, long, parse(try_from_str))]
    nvr: reqwest::Url,

    #[structopt(long)]
    dry_run: bool,
}

fn retry_forever<F>(name: String, mut f: F)
where F: FnMut() -> Result<(), Error> {
    loop {
        if let Err(e) = f() {
            error!("{}: {:?}", &name, e);
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
}

/// Retries HTTP and server errors for a while.
async fn retry_http<T, F>(desc: &str, mut f: impl FnMut() -> F) -> Result<T, Error>
where F: Future<Output = Result<T, Error>> {
    const MAX_ATTEMPTS: usize = 60;
    let mut attempt = 1;
    loop {
        let e = match f().await {
            Ok(t) => return Ok(t),
            Err(e) => e,
        };
        let re = match e.downcast_ref::<reqwest::Error>() {
            None => return Err(e),
            Some(e) => e,
        };
        if matches!(re.status(), Some(s) if s.is_client_error()) {
            return Err(e);
        }
        warn!("{}: attempt {}/{}: {}", desc, attempt, MAX_ATTEMPTS, e);
        if attempt == 60 {
            return Err(e.context(format!("Last of {} attempts", attempt)).into());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
        attempt += 1;
    }
}

/// Sends the request with a timeout just for getting a `Response`.
/// Unlike [reqwest::RequestBuilder::timeout], this does not apply to finishing the
/// response body. That wouldn't be very useful for never-ending multipart streams.
fn send_with_timeout(timeout: std::time::Duration, builder: reqwest::RequestBuilder) -> impl Future<Output = Result<reqwest::Response, Error>> {
    tokio::time::timeout(timeout, builder.send())
        .map_ok_or_else(
            |elapsed| Err(format_err!("connect timeout after {}", elapsed)),
            |connect_result| connect_result.map_err(Error::from))
}

fn init_logging() -> mylog::Handle {
    let h = mylog::Builder::new()
        .set_format(::std::env::var("MOONFIRE_FORMAT")
                    .map_err(|_| ())
                    .and_then(|s| mylog::Format::from_str(&s))
                    .unwrap_or(mylog::Format::Google))
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
        .build();
    h.clone().install().unwrap();
    h
}

#[tokio::main]
async fn main() {
    let mut h = init_logging();
    let _a = h.async_scope();
    let opt = Opt::from_args();
    let nvr: &'static _ = Box::leak(Box::new(moonfire_nvr_client::Client::new(opt.nvr, opt.cookie)));
    let top_level = retry_http("get camera configs from nvr",
                               || nvr.top_level(&moonfire_nvr_client::TopLevelRequest {
        days: false,
        camera_configs: true,
    })).await.unwrap();

    let dry_run = opt.dry_run;
    let dahua_uuid = Uuid::parse_str("ee66270f-d9c6-4819-8b33-9720d4cbca6b").unwrap();
    let hikvision_uuid = Uuid::parse_str("18bf0756-2120-4fbc-99d1-a367b10ef297").unwrap();
    let rtsp_uuid = Uuid::parse_str("5684523f-f29d-42e9-b6af-1e123f2b76fb").unwrap();

    let mut cameras_by_uuid: FnvHashMap<_, moonfire_nvr_client::Camera> =
        FnvHashMap::with_capacity_and_hasher(top_level.cameras.len(), Default::default());
    for c in &top_level.cameras {
        cameras_by_uuid.insert(c.uuid, (*c).clone());
    }
    let cameras_by_uuid: &'static _ = Box::leak(Box::new(cameras_by_uuid));
    let mut handles = Vec::new();
    for s in &top_level.signals {
        let c = match cameras_by_uuid.get(&s.source) {
            None => continue,
            Some(c) => c,
        };
        let s_id = s.id;
        if s.type_ == rtsp_uuid {
            let name = c.short_name.clone();
            info!("watching rtsp camera {}", &name);
            handles.push(tokio::task::spawn_blocking({
                let nvr = nvr.clone();
                move || {
                    let mut w = rtsp::Watcher::new(name.clone(), c, nvr, s_id).unwrap();
                    retry_forever(name, move || w.watch_once());
                }
            }));
        } else if s.type_ == dahua_uuid {
            let name = c.short_name.clone();
            info!("watching dahua camera {}", &name);
            handles.push(tokio::spawn(async move {
                let mut w = dahua::Watcher::new(
                    name.clone(), c.config.as_ref().unwrap(), nvr, dry_run, s_id).unwrap();
                loop {
                    if let Err(e) = w.watch_once().await {
                        error!("{}: {:?}", &name, e);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }));
        } else if s.type_ == hikvision_uuid {
            let name = c.short_name.clone();
            info!("watching hikvision camera {}", &name);
            handles.push(tokio::spawn(async move {
                let mut w = hikvision::Watcher::new(
                    name.clone(), c.config.as_ref().unwrap(), nvr, dry_run, s_id).unwrap();
                loop {
                    if let Err(e) = w.watch_once().await {
                        error!("{}: {:?}", &name, e);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }));
        }
    }
    info!("all running");
    futures::future::join_all(handles).await;
}
