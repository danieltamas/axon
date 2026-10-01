//! Stop doorbells (BUS-PLAN §6): one file per stopped agent next to `axon.db`, so a pending
//! stop still denies when the database is unreadable or removed mid-session.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// `<data>/axon/doorbells`, beside the database file.
pub fn dir(db: &Path) -> PathBuf {
    db.with_file_name("doorbells")
}

/// Agent ids come from hook payloads, so anything but a plain lowercase name is hashed.
/// Lowercase keeps `Worker` and `worker` apart on case-insensitive filesystems, and the
/// `=` of a hashed name never occurs in a plain one, so the two kinds cannot collide.
fn path(db: &Path, agent: &str) -> PathBuf {
    let plain = !agent.is_empty()
        && !agent.starts_with('.')
        && agent
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    let name = if plain {
        agent.to_owned()
    } else {
        format!("={}", blake3::hash(agent.as_bytes()).to_hex())
    };
    dir(db).join(format!("{name}.json"))
}

pub fn ring(db: &Path, agent: &str, reason: &str) -> anyhow::Result<()> {
    let dir = dir(db);
    std::fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    std::fs::write(path(db, agent), json!({"reason": reason}).to_string())?;
    Ok(())
}

pub fn clear(db: &Path, agent: &str) -> anyhow::Result<()> {
    match std::fs::remove_file(path(db, agent)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// The stop reason for `agent`, when its doorbell is rung. An unreadable doorbell still
/// counts: failing closed is the point.
pub fn reason(db: &Path, agent: &str) -> Option<String> {
    let reason = match std::fs::read(path(db, agent)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => None,
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|v| v["reason"].as_str().map(str::to_owned)),
    };
    Some(reason.unwrap_or_else(|| "a stop is pending for this agent (axon-bus)".to_owned()))
}
