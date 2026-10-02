//! Work order retained §12 migration: preserve old-main messages while admitting peer senders.
mod common;
use common::*;
use rusqlite::{params, types::Value, Connection};
use std::time::Duration;

const OLD_SCHEMA: &str = include_str!("../../../tests/fixtures/migrations/pre_federation_bus.sql");
const COLUMNS: &str = "id,thread,seq,from_id,to_id,kind,body,refs_json,needs_reply,deadline,default_reply,sent_at,delivered_at,acked_at";

fn messages(conn: &Connection) -> Vec<Vec<Value>> {
    conn.prepare(&format!("SELECT {COLUMNS} FROM messages ORDER BY id"))
        .unwrap()
        .query_map([], |row| (0..14).map(|i| row.get(i)).collect())
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn old_main_message_fk_migrates_without_loss_and_enforces_local_sender_integrity() {
    let bus = Bus::with_limit(Duration::from_secs(15));
    std::fs::create_dir_all(bus.db_path().parent().unwrap()).unwrap();
    let legacy = Connection::open(bus.db_path()).unwrap();
    legacy.pragma_update(None, "foreign_keys", "ON").unwrap();
    legacy.execute_batch(OLD_SCHEMA).unwrap();
    for id in ["sender", "recipient"] {
        legacy
            .execute(
                "INSERT INTO agents (id,harness,session_id,root_id,status,started_at,last_seen_at)
            VALUES (?1,'claude',?1,?1,'idle',1,1)",
                [id],
            )
            .unwrap();
    }
    for (id, seq, body, refs, delivered, acked) in [
        (
            "old-question",
            1,
            "line one\n\t🦀 \"quoted\"",
            r#"["src/main.rs:L1"]"#,
            None,
            None,
        ),
        (
            "old-answer",
            2,
            "answer\0with embedded NUL",
            r#"["old-question"]"#,
            Some(42i64),
            Some(43i64),
        ),
    ] {
        legacy.execute(&format!("INSERT INTO messages ({COLUMNS})
            VALUES (?1,'old-thread',?2,'sender','recipient',?3,?4,?5,?6,123456789,'fallback',123456,?7,?8)"),
            params![id, seq, if seq == 1 { "question" } else { "answer" }, body, refs,
                seq == 1, delivered, acked]).unwrap();
    }
    assert_eq!(legacy.query_row(
        "SELECT count(*) FROM pragma_foreign_key_list('messages') WHERE \"from\"='from_id' AND \"table\"='agents'",
        [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let before = messages(&legacy);
    drop(legacy);

    bus.init(); // The new executable, not a Rust migration function, opens the old database.
    let migrated = bus.db();
    migrated.pragma_update(None, "foreign_keys", "ON").unwrap();
    assert_eq!(
        messages(&migrated),
        before,
        "every old column survives byte for byte, including NULLs"
    );
    assert_eq!(
        migrated
            .query_row(
                "SELECT count(*) FROM pragma_foreign_key_list('messages') WHERE \"from\"='from_id'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let insert = "INSERT INTO messages (id,thread,seq,from_id,to_id,kind,body)
        VALUES (?1,'new-thread',1,?2,'recipient','sync','migration probe')";
    let ghost = migrated.execute(insert, params!["ghost-row", "nonexistent-local-sender"]);
    assert!(
        matches!(ghost, Err(rusqlite::Error::SqliteFailure(ref error, _))
        if error.code == rusqlite::ErrorCode::ConstraintViolation),
        "local ghost sender was accepted: {ghost:?}"
    );
    let update = migrated.execute(
        "UPDATE messages SET from_id='nonexistent-local-sender' WHERE id='old-question'",
        [],
    );
    assert!(
        matches!(update, Err(rusqlite::Error::SqliteFailure(ref error, _))
        if error.code == rusqlite::ErrorCode::ConstraintViolation),
        "local ghost UPDATE was accepted: {update:?}"
    );
    assert_eq!(
        messages(&migrated),
        before,
        "refused writes leave old rows unchanged"
    );
    migrated
        .execute(insert, params!["peer-row", "peer:alice/abcdefghijkl"])
        .unwrap();
    let after = messages(&migrated);
    assert_eq!(after.len(), before.len() + 1);
    assert_eq!(
        migrated
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                .get::<_, i64>(
                0
            ))
            .unwrap(),
        0
    );
    drop(migrated);
    bus.init();
    assert_eq!(
        messages(&bus.db()),
        after,
        "reopening the migrated database is idempotent"
    );
}
