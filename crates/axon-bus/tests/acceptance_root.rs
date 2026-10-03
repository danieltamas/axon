//! BUG-1/REL-1/CDX-1 and REL-2/TEST-1: start the shipped root CLI, not `axon-bus serve`.
mod common;
use common::*;
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn root_dashboard_stays_alive_and_serves_health_and_summary() {
    let bus = Bus::with_limit(Duration::from_secs(15));
    // Root CLI has no ready-file flag. Reserve a free port, then poll that listener.
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let binary = std::path::Path::new(assert_cmd::cargo::cargo_bin!("axon-bus"))
        .with_file_name(format!("axon{}", std::env::consts::EXE_SUFFIX));
    let mut command = Command::new(binary);
    bus.isolate(&mut command);
    command
        .args([
            "--port",
            &address.port().to_string(),
            "--no-open",
            "--no-hooks",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    drop(reservation);
    let mut server = Server {
        process: Running(
            command
                .spawn()
                .expect("workspace root axon binary must be built"),
        ),
        address,
        url: format!("http://{address}"),
        cookie: String::new(),
        token: String::new(),
    };
    // The budget declared above: a freshly linked binary is checked by macOS on its first
    // launch, which alone can take seconds under a full suite.
    let deadline = Instant::now() + bus.remaining();
    loop {
        if server.process.0.try_wait().unwrap().is_some() {
            let output = server.process.finish(Duration::from_secs(1));
            panic!(
                "root axon exited before listening: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "root axon never opened its dashboard"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    (server.cookie, server.token) = server.login(&bus);
    for path in ["/api/health", "/api/summary"] {
        let response = server.request(&bus, "GET", path, &[], "");
        assert_eq!(response.status, 200, "root {path}");
        parse_json(&response.body);
    }
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        server.process.0.try_wait().unwrap().is_none(),
        "root axon exited after startup"
    );
    assert_eq!(
        server.request(&bus, "GET", "/api/health", &[], "").status,
        200
    );
    assert_eq!(
        server.request(&bus, "GET", "/api/summary", &[], "").status,
        200
    );
}
