//! Remote send targets (docs/P2P-SPEC.md §7): `peer:<label>/<session>`. Only the refusals a
//! node can decide today live here; queuing and the membership checks arrive with discovery.

use rusqlite::{Connection, OptionalExtension};

use super::enabled;

pub const TARGET_PREFIX: &str = "peer:";

/// Why a send to `to` is refused, or `None` when `to` is not a remote target and the local
/// send path applies. Every remote target is refused for now: no peer shares anything yet, so
/// no remote session is known (`unknown_session`).
pub fn refusal(conn: &Connection, to: &str) -> rusqlite::Result<Option<&'static str>> {
    let Some(target) = to.strip_prefix(TARGET_PREFIX) else {
        return Ok(None);
    };
    if !enabled(conn) {
        return Ok(Some("federation_off"));
    }
    let label = target.split('/').next().unwrap_or_default();
    // Prefer the live row: a removed peer's label may have been reused since.
    let state: Option<String> = conn
        .query_row(
            "SELECT state FROM peers WHERE label=?1 ORDER BY state='removed', paired_at DESC",
            [label],
            |r| r.get(0),
        )
        .optional()?;
    Ok(Some(match state.as_deref() {
        None => "unknown_peer",
        Some("removed") => "peer_removed",
        Some("paused") => "peer_paused",
        Some(_) => "unknown_session",
    }))
}
