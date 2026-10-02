//! RR-3/RR-4 supersede the old lost-resume limitation in P2P-SPEC section 12.
mod common;
use common::fed::*;
use serde_json::json;
use std::time::{Duration, Instant};

fn action(pair: &Pair, on_a: bool, verb: &str) {
    let (bus, server, id) = if on_a {
        (&pair.a, &pair.sa, &pair.pa)
    } else {
        (&pair.b, &pair.sb, &pair.pb)
    };
    api(
        bus,
        server,
        "POST",
        &format!("/api/fed/peers/{id}/{verb}"),
        json!({}),
    );
}

fn generation(bus: &common::Bus, id: &str) -> i64 {
    db(bus)
        .query_row("SELECT generation FROM peers WHERE peer_id=?1", [id], |r| {
            r.get(0)
        })
        .unwrap()
}

fn reconciled(pair: &Pair, generations: (i64, i64)) {
    // section 12 caps dial backoff at 10 s; allow three cycles, independent of setup time.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let a = peer(&pair.a, &pair.sa, &pair.pa);
        let b = peer(&pair.b, &pair.sb, &pair.pb);
        assert!(
            Instant::now() < deadline,
            "RR-3/RR-4: lifecycle did not reconcile: A={a}, B={b}"
        );
        if a["state"] == "connected"
            && b["state"] == "connected"
            && a["remote_paused"] == false
            && b["remote_paused"] == false
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        (generation(&pair.a, &pair.pa), generation(&pair.b, &pair.pb)),
        generations
    );
    for bus in [&pair.a, &pair.b] {
        assert_eq!(
            count(bus, "SELECT count(*) FROM peers"),
            1,
            "no replacement pairing"
        );
        assert_eq!(
            count(bus, "SELECT count(*) FROM peers WHERE state='active'"),
            1
        );
    }
}

#[test]
fn both_paused_peers_resume_to_connected_without_repairing() {
    let pair = Pair::paired(Duration::from_secs(90));
    let generations = (generation(&pair.a, &pair.pa), generation(&pair.b, &pair.pb));
    action(&pair, true, "pause");
    eventually(&pair.b, "B observes A pause", || {
        peer(&pair.b, &pair.sb, &pair.pb)["remote_paused"] == true
    });
    action(&pair, false, "pause");
    assert_eq!(peer(&pair.a, &pair.sa, &pair.pa)["state"], "paused");
    assert_eq!(peer(&pair.b, &pair.sb, &pair.pb)["state"], "paused");
    std::thread::scope(|scope| {
        let a = scope.spawn(|| action(&pair, true, "resume"));
        let b = scope.spawn(|| action(&pair, false, "resume"));
        a.join().unwrap();
        b.join().unwrap();
    });
    reconciled(&pair, generations);
}

#[cfg(unix)]
#[test]
fn lost_resume_notice_reconciles_remote_paused_after_the_other_owner_resumes() {
    let mut pair = Pair::paired(Duration::from_secs(90));
    let generations = (generation(&pair.a, &pair.pa), generation(&pair.b, &pair.pb));
    action(&pair, true, "pause");
    eventually(&pair.b, "B remembers A pause", || {
        peer(&pair.b, &pair.sb, &pair.pb)["remote_paused"] == true
    });
    action(&pair, false, "pause");
    action(&pair, false, "resume");
    // A still refuses inbound connections. B's best-effort resumed notice cannot be required.
    assert_eq!(peer(&pair.a, &pair.sa, &pair.pa)["state"], "paused");
    assert_eq!(peer(&pair.b, &pair.sb, &pair.pb)["remote_paused"], true);
    std::thread::sleep(Duration::from_secs(3));
    // Lose A's notice too, then discard B's sockets so buffered notices cannot heal the test.
    let stopped = Stopped::new(&pair.sb);
    action(&pair, true, "resume");
    pair.sb.process.0.kill().unwrap();
    pair.sb.process.0.wait().unwrap();
    drop(stopped);
    pair.sb = common::Server::start(&pair.b, true);
    reconciled(&pair, generations);
}
