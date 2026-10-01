//! The running federation service as the dashboard sees it: one slot that boot and
//! Settings both fill through `restart`, so there is a single place that starts or stops it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::fed::service::{self, Handle};

#[derive(Clone)]
pub struct Federation {
    db: PathBuf,
    running: Arc<Mutex<Option<Handle>>>,
}

impl Federation {
    pub fn new(db: &Path) -> Self {
        Self {
            db: db.to_owned(),
            running: Arc::default(),
        }
    }

    /// Stop the service if it runs, then start it from the stored settings (which leaves it
    /// stopped when federation is off). Holding the slot throughout keeps two callers from
    /// racing for the endpoint and the data dir's lock.
    pub async fn restart(&self) {
        let mut running = self.running.lock().await;
        if let Some(old) = running.take() {
            old.shutdown().await;
        }
        *running = match self.db.parent() {
            Some(data_dir) => service::start(data_dir, &self.db).await,
            None => None,
        };
    }

    pub async fn shutdown(&self) {
        if let Some(handle) = self.running.lock().await.take() {
            handle.shutdown().await;
        }
    }
}
