use std::collections::{HashMap, HashSet, hash_map::RandomState};

use failure::{Error, format_err};
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
            let toplevel_resp = cli.top_level(&client::TopLevelRequest {
                days: false,
                camera_configs: false,
            }).await?;
            let mut signal_types = HashMap::new();
            for t in &toplevel_resp.signal_types {
                let mut state_names = Vec::new();
                for s in &t.states {
                    let v = usize::from(s.value);
                    if v >= state_names.len() {
                        state_names.resize(v + 1, None);
                    }
                    state_names[v] = Some(s.name.as_str());
                }
                signal_types.insert(t.uuid, state_names);
            }
            let mut signals_by_id = HashMap::new();
            for s in &toplevel_resp.signals {
                signals_by_id.insert(s.id, s);
            }
            let signals_resp = cli.signals(&client::SignalsRequest {
                start,
                end,
            }).await?;
            let signals = signals.map(|v| HashSet::<u32, RandomState>::from_iter(v.into_iter()));
            for i in 0..signals_resp.signal_ids.len() {
                let signal_id = signals_resp.signal_ids[i];
                let state_val = usize::from(signals_resp.states[i]);
                let time_90k = signals_resp.times_90k[i];
                if let Some(s) = signals.as_ref() {
                    if !s.contains(&signal_id) {
                        continue;
                    }
                }
                let signal = signals_by_id.get(&signal_id).ok_or_else(|| format_err!("unknown signal {}", signal_id))?;
                let state_names = signal_types.get(&signal.type_).ok_or_else(|| format_err!("signal {} references unknown type {}", &signal.short_name, signal.type_))?;
                let state_name = match state_val {
                    0 => Some("unknown"),
                    i if i < state_names.len() => state_names[i],
                    _ => None,
                };
                let state_name = state_name.ok_or_else(|| format_err!("signal {} state {} unknown", &signal.short_name, state_val))?;
                println!("{}: {}: {}", time_90k, &signal.short_name, state_name);
           }
        },
    }
    Ok(())
}