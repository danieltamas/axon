//! The running federation service as the dashboard sees it: one slot that boot and
//! Settings both fill through `restart`, so there is a single place that starts or stops it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, RwLock};

use crate::fed;
use crate::fed::service::{self, Handle};
use crate::store;

/// How often the owner compares the stored settings with what it runs. Another process (the
/// CLI) may change them, and the owner has to follow within 2 s.
const WATCH_EVERY: Duration = Duration::from_secs(1);

/// What a running service was started from.
type Applied = (bool, String);

#[derive(Clone)]
pub struct Federation {
    db: PathBuf,
    /// `Shared::transmit_gate` of every service generation, owned here so it survives restarts.
    transmit_gate: Arc<RwLock<()>>,
    running: Arc<Mutex<(Option<Handle>, Option<Applied>)>>,
    watching: Arc<AtomicBool>,
    closed: Arc<AtomicBool>,
}

impl Federation {
    pub fn new(db: &Path) -> Self {
        Self {
            db: db.to_owned(),
            transmit_gate: Arc::default(),
            running: Arc::default(),
            watching: Arc::default(),
            closed: Arc::default(),
        }
    }

    /// Stop the service if it runs, then start it from the stored settings (which leaves it
    /// stopped when federation is off). Holding the slot throughout keeps two callers from
    /// racing for the endpoint and the data dir's lock. The first call also starts following
    /// the stored settings, so a change made by another process takes effect here.
    pub async fn restart(&self) {
        self.apply(true).await;
        if !self.watching.swap(true, Ordering::SeqCst) {
            tokio::spawn(self.clone().follow_settings());
        }
    }

    async fn apply(&self, force: bool) {
        let mut slot = self.running.lock().await;
        let stored = self.stored_settings().await;
        if !force && slot.1 == stored {
            return;
        }
        if let Some(old) = slot.0.take() {
            old.shutdown().await;
        }
        slot.1 = stored;
        slot.0 = match self.db.parent() {
            Some(data_dir) => {
                service::start_with_gate(data_dir, &self.db, self.transmit_gate.clone()).await
            }
            None => None,
        };
        if let Some(handle) = slot.0.as_ref() {
            fed::install(handle, &self.db);
        }
    }

    async fn stored_settings(&self) -> Option<Applied> {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = store::open(&db).ok()?;
            Some((fed::enabled(&conn), fed::relay_setting(&conn)))
        })
        .await
        .ok()
        .flatten()
    }

    async fn follow_settings(self) {
        while !self.closed.load(Ordering::SeqCst) {
            tokio::time::sleep(WATCH_EVERY).await;
            if self.closed.load(Ordering::SeqCst) {
                break;
            }
            // An unreadable database leaves the service as it is.
            if self.stored_settings().await.is_some() {
                self.apply(false).await;
            }
        }
    }

    /// The gate a revocation takes exclusively before it commits; the same one every running
    /// service's outbox shares, whether or not a service runs at this moment.
    pub fn transmit_gate(&self) -> Arc<RwLock<()>> {
        self.transmit_gate.clone()
    }

    /// The running service, or `None` when federation is off or did not start.
    pub async fn handle(&self) -> Option<Handle> {
        self.running.lock().await.0.clone()
    }

    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::SeqCst);
        if let Some(handle) = self.running.lock().await.0.take() {
            handle.shutdown().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn becomes(federation: &Federation, running: bool) -> bool {
        for _ in 0..40 {
            if federation.handle().await.is_some() == running {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[tokio::test]
    async fn a_setting_changed_by_another_process_is_followed_within_the_deadline() {
        std::env::set_var("AXON_FED_RELAY", "disabled");
        std::env::set_var("AXON_FED_BIND", "127.0.0.1:0");
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("axon.db");
        let conn = store::init(&db).unwrap();
        let federation = Federation::new(&db);
        federation.restart().await;
        assert!(federation.handle().await.is_none(), "off by default");
        fed::enable(dir.path(), &conn, true).unwrap();
        assert!(becomes(&federation, true).await, "enabling is picked up");
        fed::enable(dir.path(), &conn, false).unwrap();
        assert!(becomes(&federation, false).await, "disabling is picked up");
        federation.shutdown().await;
    }
}
