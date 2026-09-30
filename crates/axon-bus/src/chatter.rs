//! The chatter view (BUS-PLAN §7): recent bus messages and the live root links, which the
//! page draws along the tree.

use rusqlite::Connection;
use serde_json::{json, Value};

use crate::store::now_ms;

/// Messages per snapshot; the thread timeline shows no more.
const RECENT_MESSAGES: i64 = 60;

/// The newest messages, oldest first.
pub fn messages(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT id,thread,seq,from_id,to_id,kind,body,needs_reply,sent_at,delivered_at,acked_at,
                refs_json
         FROM (SELECT rowid AS n, * FROM messages ORDER BY rowid DESC LIMIT ?1) ORDER BY n",
    )?;
    let rows = stmt.query_map([RECENT_MESSAGES], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "thread": r.get::<_, String>(1)?,
            "seq": r.get::<_, i64>(2)?,
            "from": r.get::<_, String>(3)?,
            "to": r.get::<_, String>(4)?,
            "kind": r.get::<_, String>(5)?,
            "body": r.get::<_, String>(6)?,
            "needs_reply": r.get::<_, bool>(7)?,
            "sent_at": r.get::<_, Option<i64>>(8)?,
            "delivered_at": r.get::<_, Option<i64>>(9)?,
            "acked_at": r.get::<_, Option<i64>>(10)?,
            "refs": serde_json::from_str::<Value>(&r.get::<_, String>(11)?).unwrap_or_default(),
        }))
    })?;
    rows.collect()
}

/// Root ↔ root links, each pair once.
pub fn links(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT min(from_id,to_id), max(from_id,to_id) FROM edges
         WHERE kind='link' AND (expires_at IS NULL OR expires_at>?1)",
    )?;
    let rows = stmt.query_map([now_ms()], |r| {
        Ok(json!({"a": r.get::<_, String>(0)?, "b": r.get::<_, String>(1)?}))
    })?;
    rows.collect()
}
