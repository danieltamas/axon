//! Ending open harness sessions from the dashboard (`POST /api/end`). A session seen only
//! from its process has no bus edge a stop could travel, and an idle one never reaches the
//! gate, so the operator's way to close it is the signal its terminal would send.
//!
//! Every target is checked against a fresh process sample at request time: only a live,
//! top-level harness session (`<harness>-<pid>`, the id the snapshot shows) that started
//! when the page saw it start is signalled, so a stale page, a reused pid or a forged id
//! never reaches another process.

use std::path::Path;

use serde_json::{json, Value};

use crate::memory::Sampler;
use crate::store;

/// More than any project has open; bounds the work one request can ask for.
pub const MAX_AT_ONCE: usize = 64;

/// One session to end: its snapshot id and its process start (`started_ms`).
#[derive(serde::Deserialize)]
pub struct Target {
    pub id: String,
    pub started_ms: i64,
}

pub fn end(db: &Path, targets: &[Target]) -> anyhow::Result<Value> {
    let conn = store::open(db)?;
    let mut sampler = Sampler::new();
    sampler.sample(&conn)?;
    let mut ended = Vec::new();
    let mut refused = Vec::new();
    for Target { id, started_ms } in targets {
        let pid = sampler
            .sessions()
            .iter()
            .find(|s| format!("{}-{}", s.harness, s.pid) == *id && s.started_ms == *started_ms)
            .map(|s| s.pid);
        match pid {
            Some(pid) if sampler.terminate(pid, *started_ms) => ended.push(id.clone()),
            Some(_) => refused.push(json!({"id": id, "why": "the process refused the signal"})),
            None => refused.push(json!({"id": id, "why": "no open session with this id"})),
        }
    }
    Ok(json!({"ended": ended, "refused": refused}))
}
