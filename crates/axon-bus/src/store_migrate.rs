//! One-time schema migrations for databases created by earlier builds.

use rusqlite::Connection;

/// Development builds of the owner login named both secret columns `hash`, and
/// `CREATE TABLE IF NOT EXISTS` would keep those tables. They hold only short-lived secrets, so
/// they are dropped and recreated by `SCHEMA`; open browsers sign in again.
pub(super) fn drop_stale_login_tables(conn: &Connection) -> rusqlite::Result<()> {
    for (table, column) in [
        ("login_nonces", "nonce_hash"),
        ("dashboard_sessions", "session_hash"),
    ] {
        let stale: bool = conn.query_row(
            "SELECT count(*) > 0 AND coalesce(sum(name = ?2), 0) = 0 FROM pragma_table_info(?1)",
            [table, column],
            |r| r.get(0),
        )?;
        if stale {
            // Identifiers come from the constant list above, never from input.
            conn.execute_batch(&format!("DROP TABLE {table}"))?;
        }
    }
    Ok(())
}

/// A database made before federation has `messages.from_id REFERENCES agents(id)`, which a remote
/// sender cannot satisfy. SQLite cannot drop a constraint, so the table is rebuilt once; the
/// triggers in `SCHEMA` take over the check.
pub(super) fn drop_sender_foreign_key(conn: &Connection) -> rusqlite::Result<()> {
    let keyed: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_list('messages') WHERE \"from\"='from_id')",
        [],
        |r| r.get(0),
    )?;
    if !keyed {
        return Ok(());
    }
    // The pragma is a no-op inside a transaction.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let rebuilt = conn.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE messages_new (
             id TEXT PRIMARY KEY, thread TEXT NOT NULL, seq INTEGER NOT NULL, from_id TEXT NOT NULL,
             to_id TEXT NOT NULL REFERENCES agents(id), kind TEXT NOT NULL, body TEXT NOT NULL,
             refs_json TEXT NOT NULL DEFAULT '[]', needs_reply INTEGER NOT NULL DEFAULT 0,
             deadline INTEGER, default_reply TEXT, sent_at INTEGER, delivered_at INTEGER, acked_at INTEGER
         );
         INSERT INTO messages_new (id,thread,seq,from_id,to_id,kind,body,refs_json,needs_reply,
                                   deadline,default_reply,sent_at,delivered_at,acked_at)
             SELECT id,thread,seq,from_id,to_id,kind,body,refs_json,needs_reply,
                    deadline,default_reply,sent_at,delivered_at,acked_at FROM messages;
         DROP TABLE messages;
         ALTER TABLE messages_new RENAME TO messages;
         COMMIT;",
    )
    .inspect_err(|_| drop(conn.execute_batch("ROLLBACK")));
    conn.pragma_update(None, "foreign_keys", "ON")?;
    rebuilt?;
    // Indexes and triggers went with the old table.
    conn.execute_batch(super::SCHEMA)
}

/// Columns added after their table first shipped (`CREATE TABLE IF NOT EXISTS` skips them).
pub(super) fn add_missing_columns(conn: &Connection) -> rusqlite::Result<()> {
    for (table, column, kind) in [
        ("agents", "effort", "TEXT"),
        ("agents", "introduced_at", "INTEGER"),
        ("usage", "received_at", "INTEGER"),
        ("messages", "sent_at", "INTEGER"),
        ("fed_inbox", "share_id", "TEXT"),
        ("peers", "remote_paused", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1) WHERE name=?2)",
            [table, column],
            |r| r.get(0),
        )?;
        if !exists {
            // Identifiers come from the constant list above, never from input.
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {kind}"))?;
        }
    }
    Ok(())
}
