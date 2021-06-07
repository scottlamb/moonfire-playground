//! RTSP client examples.

mod timedump;
mod timestats;

use failure::Error;
use log::{error, info};
use std::{fmt::Write, str::FromStr};
use structopt::StructOpt;

#[derive(StructOpt)]
enum Cmd {
    Timedump(timedump::Opts),
    Timestats(timestats::Opts),
}

/// Returns a pretty-and-informative version of `e`.
pub fn prettify_failure(e: &failure::Error) -> String {
    let mut msg = e.to_string();
    for cause in e.iter_causes() {
        write!(&mut msg, "\ncaused by: {}", cause).unwrap();
    }
    if e.backtrace().is_empty() {
        write!(
            &mut msg,
            "\n\n(set environment variable RUST_BACKTRACE=1 to see backtraces)"
        )
        .unwrap();
    } else {
        write!(&mut msg, "\n\nBacktrace:\n{}", e.backtrace()).unwrap();
    }
    msg
}

fn init_logging() -> mylog::Handle {
    let h = mylog::Builder::new()
        .set_format(::std::env::var("MOONFIRE_FORMAT")
                    .map_err(|_| ())
                    .and_then(|s| mylog::Format::from_str(&s))
                    .unwrap_or(mylog::Format::Google))
        .set_spec(::std::env::var("MOONFIRE_LOG").as_deref().unwrap_or("info"))
        .build();
    h.clone().install().unwrap();
    h
}

#[tokio::main]
async fn main() {
    let mut h = init_logging();
    if let Err(e) = { let _a = h.async_scope(); main_inner().await } {
        error!("Fatal: {}", prettify_failure(&e));
        std::process::exit(1);
    }
    info!("Done");
}

/// Interpets the `username` and `password` of a [Source].
fn creds(username: Option<String>, password: Option<String>) -> Option<retina::client::Credentials> {
    match (username, password) {
        (Some(username), Some(password)) => Some(retina::client::Credentials {
            username,
            password,
        }),
        (None, None) => None,
        _ => unreachable!(), // structopt/clap enforce username and password's mutual "requires".
    }
}

async fn main_inner() -> Result<(), Error> {
    let cmd = Cmd::from_args();
    match cmd {
        Cmd::Timedump(opts) => timedump::run(opts).await,
        Cmd::Timestats(opts) => timestats::run(opts).await,
    }
}
