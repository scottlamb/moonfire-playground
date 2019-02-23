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

extern crate base64;
extern crate bytes;
extern crate docopt;
extern crate failure;
extern crate http;
extern crate httparse;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
extern crate mime;
extern crate openssl;
extern crate pretty_hex;
extern crate regex;
extern crate reqwest;
extern crate rusqlite;
extern crate slog;
extern crate slog_envlogger;
extern crate slog_stdlog;
extern crate slog_term;
extern crate xml;

mod dahua;
mod hikvision;
mod multipart;

use docopt::Docopt;
use failure::Error;
use std::thread;

const HIK_CAMERAS: [(&'static str, &'static str); 5] = [
    ("back_west", "192.168.5.101"),
    ("back_east", "192.168.5.102"),
    ("courtyard", "192.168.5.103"),
    ("west_side", "192.168.5.104"),
    ("garage",    "192.168.5.106"),
];

const DAHUA_CAMERAS: [(&'static str, &'static str); 1] = [
    ("driveway",    "192.168.5.108"),
];

const USAGE: &'static str = "
Usage:
  camera-motion --password=PASSWORD
  camera-motion (-h | --help)
";

fn init_logging() {
    use slog::DrainExt;
    let drain = slog_term::StreamerBuilder::new().async().full().build();
    let drain = slog_envlogger::new(drain);
    slog_stdlog::set_logger(slog::Logger::root(drain.ignore_err(), None)).unwrap();
}

trait Watcher {
    fn watch_once(&self) -> Result<(), Error> ;
}

fn watch_forever(name: &'static str, w: &Watcher) {
    loop {
        if let Err(e) = w.watch_once() {
            error!("{}: {}", name, e);
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
}

fn main() {
    init_logging();
    let args = Docopt::new(USAGE).and_then(|d| d.parse()).unwrap_or_else(|e| e.exit());
    let password: &str = args.get_str("--password");
    let mut threads = Vec::new();
    for &(name, host) in &HIK_CAMERAS {
        let w = hikvision::Watcher::new(name.to_owned(), host, "admin", password).unwrap();
        threads.push(thread::Builder::new().name(name.to_owned())
                                           .spawn(move|| watch_forever(name, &w))
                                           .expect("can't create thread"));
        info!("starting thread for hikvision camera {}", name);
    }
    for &(name, host) in &DAHUA_CAMERAS {
        let w = dahua::Watcher::new(name.to_owned(), host, "admin", password).unwrap();
        threads.push(thread::Builder::new().name(name.to_owned())
                                           .spawn(move|| watch_forever(name, &w))
                                           .expect("can't create thread"));
        info!("starting thread for dahua camera {}", name);
    }
    info!("all threads started");
    for t in threads.drain(..) {
        t.join().unwrap();
    }
}
