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

#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
#[cfg_attr(test, macro_use)] extern crate serde_json;

mod dahua;
mod hikvision;
mod multipart;
mod nvr;

use docopt::Docopt;
use failure::Error;
use fnv::FnvHashMap;
use http::header::HeaderValue;
use reqwest::Url;
use std::thread;
use uuid::Uuid;

/*const HIK_CAMERAS: [(&'static str, &'static str, ); 5] = [
    // name       ip                signal_id
    ("back_west", "192.168.5.101",  1),
    ("back_east", "192.168.5.102",  2),
    ("courtyard", "192.168.5.103",  3),
    ("west_side", "192.168.5.104",  4),
    ("garage",    "192.168.5.106",  5),
];

const DAHUA_CAMERAS: [(&'static str, &'static str); 1] = [
    ("driveway",    "192.168.5.108", 6),
];*/

const USAGE: &'static str = "
Usage:
  camera-motion [--cookie=COOKIE] --nvr=URL
  camera-motion (-h | --help)
";

trait Watcher {
    fn watch_once(&mut self) -> Result<(), Error> ;
}

fn watch_forever(name: String, w: &mut Watcher) {
    loop {
        if let Err(e) = w.watch_once() {
            error!("{}: {}", &name, e);
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
}

fn parse_fmt<S: AsRef<str>>(fmt: S) -> Option<mylog::Format> {
    match fmt.as_ref() {
        "google" => Some(mylog::Format::Google),
        "google-systemd" => Some(mylog::Format::GoogleSystemd),
        _ => None,
    }
}

/// Retries HTTP and server errors for a while.
fn retry_http<T>(desc: &str, mut f: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {
    const MAX_ATTEMPTS: usize = 60;
    let mut attempt = 1;
    loop {
        let e = match f() {
            Ok(t) => return Ok(t),
            Err(e) => e,
        };
        let re = match e.downcast_ref::<reqwest::Error>() {
            None => return Err(e),
            Some(e) => e,
        };
        if !re.is_http() && !re.is_server_error() {
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

fn main() {
    let mut h = mylog::Builder::new()
        .set_format(::std::env::var("MOONFIRE_FORMAT")
                    .ok()
                    .and_then(parse_fmt)
                    .unwrap_or(mylog::Format::Google))
        .set_spec(&::std::env::var("MOONFIRE_LOG").unwrap_or("info".to_owned()))
        .build();
    h.clone().install().unwrap();
    let _a = h.r#async();
    let args = Docopt::new(USAGE).and_then(|d| d.parse()).unwrap_or_else(|e| e.exit());
    let cookie = args.find("--cookie")
                     .map(|v| HeaderValue::from_str(v.as_str()).unwrap());
    let nvr = Url::parse(args.get_str("--nvr")).unwrap();
    let nvr = Box::leak(Box::new(nvr::Client::new(nvr, cookie)));
    let top_level = retry_http("get camera configs from nvr",
                               || nvr.top_level(&nvr::TopLevelRequest {
        days: false,
        camera_configs: true,
    })).unwrap();
    let mut threads = Vec::new();

    let dahua_uuid = Uuid::parse_str("ee66270f-d9c6-4819-8b33-9720d4cbca6b").unwrap();
    let hikvision_uuid = Uuid::parse_str("18bf0756-2120-4fbc-99d1-a367b10ef297").unwrap();

    let mut cameras_by_uuid =
        FnvHashMap::with_capacity_and_hasher(top_level.cameras.len(), Default::default());
    for c in &top_level.cameras {
        cameras_by_uuid.insert(c.uuid, c);
    }
    for s in &top_level.signals {
        let c = match cameras_by_uuid.get(&s.source) {
            None => continue,
            Some(c) => c,
        };
        let config = c.config.as_ref().unwrap();
        if s.type_ == dahua_uuid {
            let name = c.short_name.clone();
            let mut w = dahua::Watcher::new(name.clone(), config, nvr, s.id).unwrap();
            info!("starting thread for dahua camera {}", &name);
            threads.push(thread::Builder::new().name(name.clone())
                                               .spawn(move || watch_forever(name, &mut w))
                                               .expect("can't create thread"));
        } else if s.type_ == hikvision_uuid {
            let name = c.short_name.clone();
            let mut w = hikvision::Watcher::new(name.clone(), config, nvr, s.id).unwrap();
            info!("starting thread for hikvision camera {}", &name);
            threads.push(thread::Builder::new().name(name.clone())
                                               .spawn(move || watch_forever(name, &mut w))
                                               .expect("can't create thread"));
        }
    }
    info!("all threads started");
    for t in threads.drain(..) {
        t.join().unwrap();
    }
}
