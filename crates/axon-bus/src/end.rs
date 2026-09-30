//! Ending open harness sessions from the dashboard (`POST /api/end`). A session seen only
//! from its process has no bus edge a stop could travel, and an idle one never reaches the
//! gate, so the operator's way to close it is the signal its terminal would send.
//!
//! Every id is checked against a fresh process sample at request time: only a live,
//! top-level harness session (`<harness>-<pid>`, the id the snapshot shows) is signalled,
//! so a stale page or a forged id can never reach another process.

use std::path::Path;

use serde_json::{json, Value};

use crate::memory::Sampler;
use crate::store;

/// More than any project has open; bounds the work one request can ask for.
pub const MAX_AT_ONCE: usize = 64;

pub fn end(db: &Path, ids: &[String]) -> anyhow::Result<Value> {
    let conn = store::open(db)?;
    let mut sampler = Sampler::new();
    sampler.sample(&conn)?;
    let mut ended = Vec::new();
    let mut refused = Vec::new();
    for id in ids {
        let session = sampler
            .sessions()
            .iter()
            .find(|s| format!("{}-{}", s.harness, s.pid) == *id);
        match session {
            Some(s) if sampler.terminate(s.pid) => ended.push(id.clone()),
            Some(_) => refused.push(json!({"id": id, "why": "the process refused the signal"})),
            None => refused.push(json!({"id": id, "why": "no open session with this id"})),
        }
    }
    Ok(json!({"ended": ended, "refused": refused}))
}
