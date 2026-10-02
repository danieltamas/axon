//! RR-16: measure the root process's listener from spawn, without a warm-up process.
mod common;
use common::*;
use std::net::{TcpListener, TcpStream};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn root_dashboard_listens_within_one_second_plus_two_hundred_ms_slack() {
    let bus = Bus::with_limit(Duration::from_secs(15));
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
    let started = Instant::now();
    let mut process = Running(
        command
            .spawn()
            .expect("prebuilt workspace root axon binary"),
    );
    let budget = Duration::from_millis(1200);
    loop {
        if process.0.try_wait().unwrap().is_some() {
            let output = process.finish(Duration::from_secs(1));
            panic!(
                "root exited before bind: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let left = budget.saturating_sub(started.elapsed());
        assert!(
            !left.is_zero(),
            "RR-16: root did not listen within 1 s + 200 ms"
        );
        if TcpStream::connect_timeout(&address, left.min(Duration::from_millis(10))).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        started.elapsed() <= budget,
        "RR-16: bind took {:?}",
        started.elapsed()
    );
    let mut server = Server {
        process,
        address,
        url: format!("http://{address}"),
        cookie: String::new(),
        token: String::new(),
    };
    // Listener latency is the timed contract; authentication happens outside that budget.
    (server.cookie, server.token) = server.login(&bus);
    assert_eq!(
        server.request(&bus, "GET", "/api/health", &[], "").status,
        200
    );
    assert!(server.process.0.try_wait().unwrap().is_none());
}
