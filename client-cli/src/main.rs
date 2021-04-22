use std::collections::{HashSet, hash_map::RandomState};

use failure::Error;
use std::iter::FromIterator;
use structopt::StructOpt;

#[derive(StructOpt)]
struct Opt {
    #[structopt(short, long, parse(try_from_str))]
    cookie: Option<reqwest::header::HeaderValue>,

    #[structopt(short, long, parse(try_from_str))]
    nvr: reqwest::Url,

    #[structopt(subcommand)]
    cmd: Command,
}

#[derive(StructOpt)]
enum Command {
    Signals {
        #[structopt(long)]
        signals: Option<Vec<u32>>,

        #[structopt(long, parse(try_from_str))]
        start: Option<base::time::Time>,

        #[structopt(long, parse(try_from_str))]
        end: Option<base::time::Time>,
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let opt = Opt::from_args();
    let cli = client::Client::new(opt.nvr, opt.cookie);
    match opt.cmd {
        Command::Signals { signals, start, end } => {
            let resp = cli.signals(&client::SignalsRequest {
                start,
                end,
            }).await?;
            let signals = signals.map(|v| HashSet::<u32, RandomState>::from_iter(v.into_iter()));
            for i in 0..resp.signal_ids.len() {
                let signal = resp.signal_ids[i];
                let state = resp.states[i];
                let time_90k = resp.times_90k[i];
                if let Some(s) = signals.as_ref() {
                    if !s.contains(&signal) {
                        continue;
                    }
                }
                println!("{}: {}: {}", time_90k, signal, state);
           }
        },
    }
    Ok(())
}