//! The EUR spend caps live in the file `axon` already reads (`src/config.rs`,
//! `budget_eur_per_*` in `config.toml`), so there is one source for the CLI and the page.
//! Edits keep the owner's other keys and comments.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use anyhow::Context;
use serde_json::{json, Value};
use toml_edit::{value, DocumentMut};

use super::input::BUDGET_FIELDS;

/// Serializes the read-modify-write of `config.toml`, so two edits never lose each other.
static WRITING: Mutex<()> = Mutex::new(());
static STAGED: AtomicU64 = AtomicU64::new(0);

pub fn config_path() -> PathBuf {
    crate::install::env_dir("XDG_CONFIG_HOME", &[".config"])
        .join("axon")
        .join("config.toml")
}

/// Missing, unreadable or malformed reads as no caps, the way `Config::load` does.
pub fn read() -> Value {
    let doc = std::fs::read_to_string(config_path())
        .ok()
        .and_then(|text| text.parse::<DocumentMut>().ok());
    let mut caps = serde_json::Map::new();
    for field in BUDGET_FIELDS {
        let item = doc
            .as_ref()
            .and_then(|doc| doc.get(&format!("budget_{field}")));
        let eur = item.and_then(|i| i.as_float().or_else(|| i.as_integer().map(|n| n as f64)));
        caps.insert(field.to_owned(), json!(eur));
    }
    Value::Object(caps)
}

/// Apply `changes` (a cap, or `None` to remove it). A file that does not parse is refused
/// rather than overwritten: the owner's hand edits are not ours to discard.
pub fn write(changes: &[(&str, Option<f64>)]) -> anyhow::Result<()> {
    let _writing = WRITING.lock().unwrap_or_else(|p| p.into_inner());
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let mut doc: DocumentMut = text
        .parse()
        .with_context(|| format!("{} is not valid TOML; fix it by hand first", path.display()))?;
    for (field, eur) in changes {
        let key = format!("budget_{field}");
        match eur {
            Some(eur) => doc[key.as_str()] = value(*eur),
            None => {
                doc.remove(&key);
            }
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    // Rename over the file so a crash never leaves half a config for `axon` to misread.
    let staged = path.with_extension(format!(
        "toml.{}.{}.tmp",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&staged, doc.to_string())?;
    std::fs::rename(&staged, &path).with_context(|| format!("replace {}", path.display()))
}
