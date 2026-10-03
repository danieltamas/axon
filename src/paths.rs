//! Where the logs Axon reads live, and where it keeps its own files.

use std::path::PathBuf;

/// The user's price override; the bundled defaults apply without one.
pub fn pricing_path() -> PathBuf {
    config_dir().join("axon").join("pricing.toml")
}

pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(Into::into)
        .unwrap_or_else(|| axon_core::home().join(".config"))
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(Into::into)
        .unwrap_or_else(|| axon_core::home().join(".local").join("share"))
}

pub fn claude_projects_dir() -> PathBuf {
    axon_core::home().join(".claude").join("projects")
}

pub fn codex_sessions_dir() -> PathBuf {
    axon_core::home().join(".codex").join("sessions")
}

pub fn opencode_dir() -> PathBuf {
    data_dir().join("opencode")
}

pub fn opencode_db_path() -> PathBuf {
    opencode_dir().join("opencode.db")
}

/// Candidate ccflare-family proxy DBs, in priority order: explicit env overrides first, then
/// the better-ccflare and ccflare defaults. Each existing one is scanned; missing ones are
/// skipped. Duplicates (same canonical path) are de-duped so a request is never counted twice.
pub fn ccflare_db_paths() -> Vec<PathBuf> {
    let mut raw: Vec<PathBuf> = Vec::new();
    for var in ["BETTER_CCFLARE_DB_PATH", "CCFLARE_DB_PATH"] {
        if let Some(p) = std::env::var_os(var) {
            raw.push(p.into());
        }
    }
    raw.push(
        config_dir()
            .join("better-ccflare")
            .join("better-ccflare.db"),
    );
    raw.push(config_dir().join("ccflare").join("ccflare.db"));
    raw.push(data_dir().join("ccflare").join("ccflare.db"));

    let mut seen = std::collections::HashSet::new();
    raw.into_iter()
        .filter(|p| {
            let key = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            seen.insert(key)
        })
        .collect()
}

/// Parent dirs of the ccflare candidate DBs, for the live file-watch.
pub fn ccflare_watch_dirs() -> Vec<PathBuf> {
    ccflare_db_paths()
        .iter()
        .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
        .collect()
}

pub fn db_path() -> PathBuf {
    axon_core::store::default_path()
}

/// Open the dashboard link in the user's browser.
pub fn open_in_browser(link: &str) {
    // A minimal Linux session (no desktop env, no xdg-settings) has xdg-open but the
    // webbrowser crate never reaches it, so it is asked first.
    #[cfg(all(unix, not(target_os = "macos")))]
    if std::process::Command::new("xdg-open")
        .arg(link)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return;
    }
    if let Err(e) = webbrowser::open(link) {
        eprintln!("axon: couldn't open a browser ({e}); open the link yourself");
    }
}
