mod cmd;
mod resp;
mod xml;

use chrono::{DateTime, FixedOffset};
use docopt::Docopt;
use failure::{Error, bail, format_err};
use url::Url;

const USAGE: &'static str = "
Usage:
  onvif --base_url=URL --username=USERNAME --password=PASSWORD
  onvif (-h | --help)
";

struct Subscription {
    client: reqwest::Client,
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
        let client = reqwest::Client::builder()
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
    let args = Docopt::new(USAGE).and_then(|d| d.parse()).unwrap_or_else(|e| e.exit());
    let base_url = Url::parse(args.get_str("--base_url")).unwrap();
    let username: &str = args.get_str("--username");
    let password: &str = args.get_str("--password");

    let mut s =
        Subscription::new(base_url.clone(), username.to_owned(), password.to_owned()).unwrap();
    for _ in 0..10 {
        dbg!(s.pull().unwrap());
    }

    let client = reqwest::Client::builder()
        .build().unwrap();
    let device_url = base_url.join("onvif/device_service").unwrap();
    let mut get_cap_resp =
        cmd::get_capabilities(&client, device_url,
                              &cmd::UsernameToken::new(username, password)).unwrap();
    dbg!(&get_cap_resp);
    ensure_url_within_base(&mut get_cap_resp.media_url, &base_url).unwrap();
    println!("{}", cmd::get_metadata_configurations(&client, get_cap_resp.media_url.clone(),
                                                   &cmd::UsernameToken::new(username, password)).unwrap());
    println!("{}", cmd::set_metadata_configuration(&client, get_cap_resp.media_url,
                                                   &cmd::UsernameToken::new(username, password)).unwrap());
}
