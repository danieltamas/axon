//! Filesystem/SQLite faults: no row seeding; read-only assertions after restoration.
#[cfg(unix)]
use super::delivery::*;
use super::fed::*;
use super::Server;
use std::time::Duration;

#[cfg(unix)]
pub fn unreadable_database() {
    use std::os::unix::fs::PermissionsExt;
    let (mut pair, _, target) = shared(45);
    pair.sa.process.0.kill().unwrap();
    pair.sa.process.0.wait().unwrap();
    let path = pair.a.db_path();
    let permissions = std::fs::metadata(&path).unwrap().permissions();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0)).unwrap();
    // Privileged runners can bypass mode bits; report the specific missing fault.
    if std::fs::File::open(&path).is_ok() {
        std::fs::set_permissions(&path, permissions).unwrap();
        eprintln!("skip unreadable-database fault: runner bypasses Unix file permissions");
        return;
    }
    let output = pair
        .a
        .cmd()
        .args([
            "send",
            "--from",
            "agent-a",
            "--to",
            &target,
            "--kind",
            "sync",
            "--body",
            "must not bypass broken storage",
        ])
        .assert()
        .get_output()
        .clone();
    std::fs::set_permissions(&path, permissions).unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).starts_with("queued "));
    assert_eq!(count(&pair.a, "SELECT count(*) FROM fed_outbox"), 0);
    assert_eq!(count(&pair.b, "SELECT count(*) FROM fed_inbox"), 0);
}

pub fn second_server_cannot_own_the_same_federation_service_lock() {
    let pair = Pair::paired(Duration::from_secs(30));
    let mut second = Server::anonymous(&pair.a, true, 0);
    second.cookie = second.login(&pair.a);
    // A second dashboard may be available, but cannot become another transport owner.
    assert!(pair.a.root.join("data/axon/fed/service.lock").exists());
    assert_eq!(peer(&pair.a, &pair.sa, &pair.pa)["state"], "connected");
    let logs = second.process.0.stderr.take().unwrap();
    second.process.0.kill().unwrap();
    second.process.0.wait().unwrap();
    use std::io::Read;
    let mut log = String::new();
    std::io::BufReader::new(logs)
        .read_to_string(&mut log)
        .unwrap();
    assert!(
        log.contains("lock") && log.contains(&pair.sa.process.0.id().to_string()),
        "lock holder must be identified: {log}"
    );
}

pub fn corrupt_database() {
    let (mut pair, _, target) = super::delivery::shared(45);
    pair.sa.process.0.kill().unwrap();
    pair.sa.process.0.wait().unwrap();
    // Corruption is confined to the disposable hub after its service has exited.
    // No SQL row or schema is edited to manufacture a successful federation state.
    std::fs::write(pair.a.db_path(), b"not a SQLite database").unwrap();
    let output = pair
        .a
        .cmd()
        .args([
            "send",
            "--from",
            "agent-a",
            "--to",
            &target,
            "--kind",
            "sync",
            "--body",
            "no direct-send fallback",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stdout).starts_with("queued "));
    assert_eq!(
        std::fs::read(pair.a.db_path()).unwrap(),
        b"not a SQLite database"
    );
    assert_eq!(count(&pair.b, "SELECT count(*) FROM fed_inbox"), 0);
}
