//! RR-2: an unrelated SQLite writer cannot extend a revoked stream's lifetime.
mod common;
use common::fed::api;
use common::sse::Events;
use common::*;
use serde_json::json;
use std::time::{Duration, Instant};

#[test]
fn revoked_stream_closes_within_one_second_while_another_connection_holds_the_writer() {
    let bus = Bus::with_limit(Duration::from_secs(20));
    bus.init();
    let mut server = Server::start(&bus, true);
    let writer = bus.db();
    let contender = bus.db();
    contender.busy_timeout(Duration::ZERO).unwrap();
    for _ in 0..3 {
        let mut stream = Events::open(&bus, &server);
        (server.cookie, server.token) = server.login(&bus);
        api(
            &bus,
            &server,
            "POST",
            "/api/settings/sessions/revoke_others",
            json!({}),
        );
        let revoked_at = Instant::now();
        // Revocation must commit first: SQLite cannot commit two simultaneous writers.
        // Keep the reservation through the assertion, so revalidation cannot bookkeep a write.
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(!writer.is_autocommit());
        let error = contender.execute_batch("BEGIN IMMEDIATE").unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy)
        );
        let remaining = Duration::from_secs(1).saturating_sub(revoked_at.elapsed());
        assert!(
            !remaining.is_zero(),
            "RR-2: writer acquisition exhausted closure deadline"
        );
        stream.assert_closed_within(remaining);
        assert!(revoked_at.elapsed() <= Duration::from_secs(1));
        assert!(
            !writer.is_autocommit(),
            "writer must stay held until SSE closes"
        );
        writer.execute_batch("ROLLBACK").unwrap();
    }
    assert!(server.process.0.try_wait().unwrap().is_none());
    assert_eq!(
        server.request(&bus, "GET", "/api/health", &[], "").status,
        200
    );
    let _surviving_session = Events::open(&bus, &server);
}
