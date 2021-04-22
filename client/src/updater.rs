//! Live updater for signals.
//! This handles keepalives and will eventually handle retries.
//! It batches together updates from all sources to reduce the number of keepalive requests.

use std::collections::BTreeMap;
use std::convert::TryFrom;
use std::sync::Arc;
use std::time::Duration;

use failure::Error;
use futures::FutureExt;
use futures::future::Either;

use log::error;

use crate::PostSignalsResponse;

const PREDICTION_LENGTH: crate::Duration = crate::Duration(30 * 90_000);
const UPDATE_INTERVAL: Duration = std::time::Duration::from_secs(15);

/// Starts a pusher.
/// Returns a cloneable `SignalUpdateSender` which can set updates and the join handle.
/// The pusher will exit after all senders are dropped and their updates have been processed.
pub fn start_pusher(client: Arc<crate::Client>) -> (SignalUpdaterSender, tokio::task::JoinHandle<()>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let h = tokio::spawn(worker(client, rx));
    (SignalUpdaterSender(tx), h)
}

#[derive(Clone)]
pub struct SignalUpdaterSender(tokio::sync::mpsc::UnboundedSender<Change>);

struct Change {
    start: tokio::time::Instant,
    set: BTreeMap<u32, u16>,
}

async fn send(client: &Arc<crate::Client>, change: &Change) -> Result<PostSignalsResponse, Error> {
    let signal_ids: Vec<u32> = change.set.keys().map(|id| *id).collect();
    let states: Vec<u16> = change.set.values().map(|state| *state).collect();
    let delta = tokio::time::Instant::now() - change.start;

    client.update_signals(&crate::PostSignalsRequest {
        signal_ids: &signal_ids,
        states: &states,
        start: crate::PostSignalsTimeBase::Now(-crate::Duration::try_from(delta)?),
        end: crate::PostSignalsTimeBase::Now(PREDICTION_LENGTH),
    }).await
}

/// Updates `base` with `delta`, returning true iff changes were made.
fn update(base: &mut BTreeMap<u32, u16>, delta: &BTreeMap<u32, u16>) -> bool {
    let mut modified = false;
    for (&k, &v) in delta {
        match base.entry(k) {
            std::collections::btree_map::Entry::Occupied(mut e) => {
                let e = e.get_mut();
                if *e != v {
                    modified = true;
                    *e = v;
                }
            },
            std::collections::btree_map::Entry::Vacant(e) => {
                modified = true;
                e.insert(v);
            }
        }
    }
    modified
}

async fn worker(client: Arc<crate::Client>, mut rx: tokio::sync::mpsc::UnboundedReceiver<Change>) {
    let mut keepalive: Option<Change> = None;
    loop {
        let f = match keepalive.as_ref() {
            None => Either::Left(rx.recv().map(Ok)),
            Some(k) => Either::Right(tokio::time::timeout_at(k.start, rx.recv())),
        };

        let change = match (f.await, keepalive.take()) {
            (Ok(None), _) => return,
            (Ok(Some(c)), None) => c,
            (Ok(Some(c)), Some(mut k)) => {
                if !update(&mut k.set, &c.set) {
                    keepalive = Some(k);
                    continue;
                }
                k.start = c.start;
                k
            },
            (Err(_), None) => unreachable!(),
            (Err(_), Some(k)) => k,
        };

        if let Err(e) = send(&client, &change).await {
            error!("Unable to send change: {}", e);
        }
        keepalive = Some(Change {
            start: tokio::time::Instant::now() + UPDATE_INTERVAL,
            set: change.set,
        });
    }
}

impl SignalUpdaterSender {
    pub fn update(&self, delta: BTreeMap<u32, u16>) {
        let change = Change {
            start: tokio::time::Instant::now(),
            set: delta,
        };
        if self.0.send(change).is_err() {
            error!("Change discarded because pusher has shut down");
        }
    }
}
