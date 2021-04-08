mod cmd;
mod resp;
mod xml;

use chrono::{DateTime, FixedOffset};
use failure::{Error, bail, format_err};
use structopt::StructOpt;
use url::Url;

#[derive(StructOpt)]
struct Opt {
    #[structopt(long, parse(try_from_str))]
    base_url: reqwest::Url,

    #[structopt(long)]
    username: String,

    #[structopt(long)]
    password: String,
}

struct Subscription {
    client: reqwest::blocking::Client,
    ref_url: Option<Url>,
    username: String,
    password: String,
    termination_time: DateTime<FixedOffset>,
}

fn ensure_url_within_base(url: &mut Url, base: &Url) -> Result<(), Error> {
    if base.scheme() != url.scheme() ||
       base.host() != url.host() {
        bail!("url {} is not within same scheme/host as base url {}", url, base);
    }
    if url.port_or_known_default() != base.port_or_known_default() {
        url.set_port(base.port()).map_err(|()| format_err!("set_port failed"))?;
    }
    Ok(())
}

impl Subscription {
    fn new(base_url: Url, username: String, password: String) -> Result<Self, Error> {
        let client = reqwest::blocking::Client::builder()
            .build()?;
        let device_url = base_url.join("onvif/device_service")?;
        let mut get_cap_resp =
            cmd::get_capabilities(&client, device_url,
                                  &cmd::UsernameToken::new(&username, &password))?;
        dbg!(&get_cap_resp);
        ensure_url_within_base(&mut get_cap_resp.events_url, &base_url)?;
        let mut sub_resp = cmd::create_pull_point_subscription(
            &client,
            get_cap_resp.events_url.clone(),
            &cmd::UsernameToken::new(&username, &password),
        )?;
        dbg!(&sub_resp);
        ensure_url_within_base(&mut sub_resp.ref_url, &base_url)?;
        Ok(Self {
            client,
            ref_url: Some(sub_resp.ref_url),
            username,
            password,
            termination_time: sub_resp.termination_time,
        })
    }

    fn pull(&mut self) -> Result<resp::PullMessagesResponse, Error> {
        let r = self.ref_url.as_ref().ok_or_else(|| format_err!("Subscription is closed"))?;
        let before = std::time::Instant::now();
        let pull_resp = cmd::pull_messages(
            &self.client,
            r.clone(),
            &cmd::UsernameToken::new(&self.username, &self.password),
        )?;
        let elapsed = std::time::Instant::now() - before;
        dbg!(elapsed);
        dbg!(pull_resp.termination_time - self.termination_time);
        self.termination_time = pull_resp.termination_time.clone();
        Ok(pull_resp)
    }

    fn close(&mut self) -> Result<(), Error> {
        if let Some(r) = self.ref_url.as_ref() {
            cmd::unsubscribe(
                &self.client,
                r.clone(),
                &cmd::UsernameToken::new(&self.username, &self.password),
            )?;
            self.ref_url = None;
        }
        Ok(())
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn main() {
    let opt = Opt::from_args();

    //let mut s =
    //    Subscription::new(opt.base_url.clone(),
    //                      opt.username.to_owned(),
    //                      opt.password.to_owned()).unwrap();
    //for _ in 0..10 {
    //    dbg!(s.pull().unwrap());
    //}

    let client = reqwest::blocking::Client::builder()
        .build().unwrap();
    let device_url = opt.base_url.join("onvif/device_service").unwrap();
    let token = cmd::UsernameToken::new(&opt.username, &opt.password);
    let mut get_cap_resp = cmd::get_capabilities(&client, device_url, &token).unwrap();
    dbg!(&get_cap_resp);
    ensure_url_within_base(&mut get_cap_resp.media_url, &opt.base_url).unwrap();
    //println!("{}", cmd::get_metadata_configurations(&client, get_cap_resp.media_url.clone(), &token).unwrap());
    //println!("{}", cmd::set_metadata_configuration(&client, get_cap_resp.media_url.clone(), &token).unwrap());
    //println!("{}", cmd::add_metadata_configuration(&client, get_cap_resp.media_url.clone(), &token).unwrap());
}
