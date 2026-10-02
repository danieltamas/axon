//! Admission rates and the memory counters behind them (docs/P2P-SPEC.md §12, C3). Nothing a
//! peer can trigger by the rate alone is written to the database: a refusal is counted here
//! and answered, and the audit takes at most a few rejection rows per peer per minute.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::identity;
use super::receive::{take, Bucket};
use super::service::FrameHandler;

/// Frames per second one peer may send of one kind, and the burst allowed.
const FRAMES_PER_SECOND: f64 = 10.0;
const FRAME_BURST: f64 = 20.0;
/// Rejections written to one database's audit per peer per window; the rest are only counted.
const REJECTIONS_PER_WINDOW: u32 = 10;
const WINDOW: Duration = Duration::from_secs(60);

/// Rate-limited and over-audit-quota rejections since start, per peer fingerprint.
static REFUSED: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(Mutex::default);
type Windows = HashMap<(String, Option<String>), (Instant, u32)>;
static AUDITED: LazyLock<Mutex<Windows>> = LazyLock::new(Mutex::default);

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

/// Count one rejection of the peer with this fingerprint that was not written to the audit.
pub fn count_refused(fingerprint: &str) {
    *locked(&REFUSED).entry(fingerprint.to_owned()).or_default() += 1;
}

/// Rejections counted for the peer with this fingerprint since the process started.
pub fn refused(fingerprint: &str) -> u64 {
    locked(&REFUSED).get(fingerprint).copied().unwrap_or(0)
}

/// `handler` behind a per-peer rate, checked before the handler does anything: a peer over
/// it gets `rate_limited` without a database write.
pub fn limited(handler: FrameHandler) -> FrameHandler {
    let buckets: Arc<Mutex<HashMap<String, Bucket>>> = Arc::default();
    Arc::new(move |node: String, frame: Value| {
        if !take(&buckets, &node, FRAMES_PER_SECOND, FRAME_BURST) {
            count_refused(
                &node
                    .parse()
                    .map_or(node.clone(), |id| identity::fingerprint(&id)),
            );
            return Box::pin(async { json!({"type": "error", "reason": "rate_limited"}) });
        }
        handler(node, frame)
    })
}

/// Whether one more rejection of this peer may be written to the audit of the database at
/// `db`. Kept in memory so deciding costs no query.
pub fn may_audit_rejection(db: Option<&str>, fingerprint: Option<&str>) -> bool {
    let key = (
        db.unwrap_or_default().to_owned(),
        fingerprint.map(str::to_owned),
    );
    let mut windows = locked(&AUDITED);
    let (since, written) = windows.entry(key).or_insert((Instant::now(), 0));
    if since.elapsed() >= WINDOW {
        (*since, *written) = (Instant::now(), 0);
    }
    *written += 1;
    *written <= REJECTIONS_PER_WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_peer_over_the_rate_is_refused_before_the_handler_runs_and_counted() {
        let reached = Arc::new(Mutex::new(0u32));
        let counted = reached.clone();
        let handler = limited(Arc::new(move |_node, _frame| {
            *locked(&counted) += 1;
            Box::pin(async { json!({"type": "ack"}) })
        }));
        let node = "rate-test-node".to_owned();
        for _ in 0..30 {
            handler(node.clone(), json!({})).await;
        }
        assert_eq!(*locked(&reached), FRAME_BURST as u32);
        assert_eq!(refused(&node), 10);
        assert_eq!(
            handler(node, json!({})).await,
            json!({"type": "error", "reason": "rate_limited"})
        );
    }

    #[test]
    fn rejections_past_the_window_budget_are_not_audited() {
        let allowed = (0..15)
            .filter(|_| may_audit_rejection(Some("db-a"), Some("fp")))
            .count();
        assert_eq!(allowed, REJECTIONS_PER_WINDOW as usize);
        assert!(
            may_audit_rejection(Some("db-b"), Some("fp")),
            "per database"
        );
    }
}
