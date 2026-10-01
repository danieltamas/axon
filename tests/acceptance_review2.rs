//! Independent acceptance checks for scan cache identity through the actual CLI.

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "axon-review2-{}-{time}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scanned_fingerprint() -> String {
    let temp = Scratch::new();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_axon"));
    command
        .env_clear()
        .current_dir(&temp.0)
        .env("HOME", &temp.0)
        .env("USERPROFILE", &temp.0)
        .env("XDG_DATA_HOME", temp.0.join("data"))
        .env("XDG_CONFIG_HOME", temp.0.join("config"))
        .env("XDG_CACHE_HOME", temp.0.join("cache"))
        .env("PATH", "")
        .args(["--scan-only", "--no-hooks"]);
    if let Some(system_root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", system_root);
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let conn = rusqlite::Connection::open(temp.0.join("data/axon/axon.db")).unwrap();
    conn.query_row(
        "SELECT value FROM scan_meta WHERE key='fingerprint'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn r7_scan_cache_key_includes_parser_version() {
    let fingerprint = scanned_fingerprint();
    assert!(
        fingerprint
            .lines()
            .any(|part| part == axon_core::ingest::PARSER_VERSION.to_string()),
        "stored scan identity lacks parser version"
    );
}

#[test]
fn r7_scan_cache_key_includes_bundled_prices() {
    assert!(
        scanned_fingerprint().contains(axon_core::pricing::BUNDLED),
        "stored scan identity lacks bundled pricing"
    );
}
