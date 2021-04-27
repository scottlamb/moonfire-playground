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

use failure::{Error, ResultExt, format_err};
use futures::TryFutureExt;
use fnv::FnvHashMap;
use serde::Deserialize;
use std::future::Future;
use std::str::FromStr;
use std::sync::Arc;
use structopt::StructOpt;

#[derive(StructOpt)]
struct Opt {
    #[structopt(long, parse(try_from_str))]
    cookie: Option<reqwest::header::HeaderValue>,

    #[structopt(long)]
    config: std::path::PathBuf,

    #[structopt(long, parse(try_from_str))]
    nvr: reqwest::Url,

    #[structopt(long)]
    dry_run: bool,
}

/// Configuration for all watchers.
#[derive(Deserialize)]
struct Config(Vec<WatcherConfig>);

/// Configuration for a single watcher.
#[derive(Deserialize)]
#[serde(rename_all="camelCase", tag="type")]
enum WatcherConfig {
    Dahua(dahua::WatcherConfig),
    Hikvision(hikvision::WatcherConfig),
    Rtsp(rtsp::WatcherConfig),
}

impl WatcherConfig {
    fn start(self, ctx: &Context) -> Result<tokio::task::JoinHandle<()>, Error> {
        match self {
            WatcherConfig::Dahua(w) => w.start(ctx),
            WatcherConfig::Hikvision(w) => w.start(ctx),
            WatcherConfig::Rtsp(w) => w.start(ctx),
        }
    }
}

pub struct Context {
    updater: moonfire_nvr_client::updater::SignalUpdaterSender,
    cameras_by_name: FnvHashMap<&'static str, &'static moonfire_nvr_client::Camera>,
    signals_by_name: FnvHashMap<&'static str, &'static moonfire_nvr_client::Signal>,
    dry_run: bool,
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
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        attempt += 1;
    }
}

fn read_config(path: &std::path::Path) -> Result<Config, Error> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    Ok(serde_json::from_reader(reader)?)
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
    if let Err(e) = main_inner().await {
        error!("Exiting due to error:\n{}", moonfire_base::prettify_failure(&e));
        std::process::exit(1);
    }
}

async fn main_inner() -> Result<(), Error> {
    let opt = Opt::from_args();
    let _ = moonfire_ffmpeg::Ffmpeg::new();
    let nvr = Arc::new(moonfire_nvr_client::Client::new(opt.nvr, opt.cookie));
    let (updater, pusher_handle) = moonfire_nvr_client::updater::start_pusher(nvr.clone());
    let mut config = read_config(&opt.config).context("while parsing config")?;
    let top_level = retry_http("get camera configs from nvr",
                               || nvr.top_level(&moonfire_nvr_client::TopLevelRequest {
        days: false,
        camera_configs: true,
    })).await.unwrap();
    let top_level = Box::leak(Box::new(top_level));

    let cameras_by_name = top_level.cameras.iter().map(|c| (c.short_name.as_str(), c)).collect();
    let signals_by_name = top_level.signals.iter().map(|c| (c.short_name.as_str(), c)).collect();
    let ctx = Context {
        updater,
        cameras_by_name,
        signals_by_name,
        dry_run: opt.dry_run,
    };
    let mut handles = Vec::new();
    handles.push(pusher_handle);
    for watcher_config in config.0.drain(..) {
        handles.push(watcher_config.start(&ctx)?);
    }
    drop(ctx.updater);
    info!("all running");
    futures::future::join_all(handles).await;
    Ok(())
}
