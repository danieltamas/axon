//! `event: fed` on `/api/stream` (P2P-SPEC §10): the body of `GET /api/fed` whenever the service
//! reports a change, and at least every few seconds, so ages stay current without database
//! writes.

use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use axum::response::sse::Event;
use futures_util::Stream;
use tokio::sync::watch;

use super::view;
use crate::fed::service::Handle;
use crate::settings::Federation;
use crate::store;

/// Under the spec's 5 s, so a slow read does not push an event past it.
const AT_LEAST_EVERY: Duration = Duration::from_secs(4);

pub fn events(
    db: PathBuf,
    federation: Federation,
) -> impl Stream<Item = Result<Event, Infallible>> {
    futures_util::stream::unfold(
        (db, federation, None, true),
        |(db, federation, mut changed, first)| async move {
            if !first {
                wait(&mut changed).await;
            }
            let handle = federation.handle().await;
            // Subscribed before the read: a change during it wakes the next wait.
            changed = handle.as_ref().map(Handle::subscribe);
            let live = handle.map(|handle| handle.health());
            let body = tokio::task::spawn_blocking({
                let db = db.clone();
                move || view(&store::open(&db)?, live)
            })
            .await;
            let data = match body {
                Ok(Ok(body)) => body.to_string(),
                _ => r#"{"error":"unavailable"}"#.to_owned(),
            };
            Some((
                Ok(Event::default().event("fed").data(data)),
                (db, federation, changed, false),
            ))
        },
    )
}

async fn wait(changed: &mut Option<watch::Receiver<u64>>) {
    let change = async {
        match changed {
            Some(rx) => drop(rx.changed().await),
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        () = change => {}
        () = tokio::time::sleep(AT_LEAST_EVERY) => {}
    }
}
