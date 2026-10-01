//! A peer's share frames (§6): `share_offer`, `share_accept`, `share_update`, `share_remove`.
//! A frame only ever touches a share that belongs to the peer it came from.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{close, get, label_ok, Share, MAX_PER_PEER};
use crate::store;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Offer {
    #[serde(rename = "type")]
    _type: String,
    v: i64,
    generation: i64,
    share_id: String,
    label: String,
    revision: i64,
    inbound: bool,
    outbound: bool,
    root_commit: Option<String>,
}

/// `share_accept` and `share_update`: the other owner's flags at a revision.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Flags {
    #[serde(rename = "type")]
    _type: String,
    v: i64,
    generation: i64,
    share_id: String,
    revision: i64,
    inbound: bool,
    outbound: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Remove {
    #[serde(rename = "type")]
    _type: String,
    v: i64,
    generation: i64,
    share_id: String,
    revision: i64,
}

fn error(reason: &str) -> Value {
    json!({"type": "error", "reason": reason})
}

fn ack(status: &str) -> Value {
    json!({"type": "ack", "status": status})
}

fn id_ok(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn revision_ok(revision: i64) -> bool {
    (1..=1 << 40).contains(&revision)
}

/// Apply one share frame from `node`. The answer is the one response frame.
pub fn apply_frame(conn: &mut Connection, node: &str, frame: Value) -> anyhow::Result<Value> {
    let kind = frame["type"].as_str().unwrap_or_default().to_owned();
    let parsed = match kind.as_str() {
        "share_offer" => serde_json::from_value::<Offer>(frame).map(Frame::Offer),
        "share_accept" | "share_update" => serde_json::from_value::<Flags>(frame)
            .map(|flags| Frame::Flags(flags, kind == "share_accept")),
        "share_remove" => serde_json::from_value::<Remove>(frame).map(Frame::Remove),
        _ => return Ok(error("unknown_frame")),
    };
    let Ok(frame) = parsed else {
        return Ok(error("bad_frame"));
    };
    let (v, generation) = frame.envelope();
    if v != 1 {
        return Ok(error("unsupported_version"));
    }
    let tx = store::write_tx(conn)?;
    let peer: Option<(String, i64)> = tx
        .query_row(
            "SELECT peer_id, generation FROM peers WHERE node_id=?1 AND state='active'",
            [node],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let reply = match peer {
        None => error("not_a_peer"),
        Some((_, current)) if current != generation => error("stale_generation"),
        Some((peer_id, _)) => frame.apply(&tx, &peer_id)?,
    };
    tx.commit()?;
    Ok(reply)
}

enum Frame {
    Offer(Offer),
    /// The flags, and whether it is an accept (else an update).
    Flags(Flags, bool),
    Remove(Remove),
}

impl Frame {
    fn envelope(&self) -> (i64, i64) {
        match self {
            Self::Offer(f) => (f.v, f.generation),
            Self::Flags(f, _) => (f.v, f.generation),
            Self::Remove(f) => (f.v, f.generation),
        }
    }

    fn apply(&self, tx: &Connection, peer_id: &str) -> anyhow::Result<Value> {
        Ok(match self {
            Self::Offer(offer) => offered(tx, peer_id, offer)?,
            Self::Flags(flags, accept) => flagged(tx, peer_id, flags, *accept)?,
            Self::Remove(remove) => removed(tx, peer_id, remove)?,
        })
    }
}

/// The share named by a frame, if it is one of this peer's.
fn owned(tx: &Connection, peer_id: &str, share_id: &str) -> rusqlite::Result<Option<Share>> {
    if !id_ok(share_id) {
        return Ok(None);
    }
    Ok(get(tx, share_id)?.filter(|share| share.peer_id == peer_id))
}

fn offered(tx: &Connection, peer_id: &str, offer: &Offer) -> anyhow::Result<Value> {
    let hint_ok = offer
        .root_commit
        .as_deref()
        .is_none_or(|c| (7..=64).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_hexdigit()));
    if !id_ok(&offer.share_id)
        || !label_ok(&offer.label)
        || !revision_ok(offer.revision)
        || !hint_ok
    {
        return Ok(error("bad_frame"));
    }
    if let Some(known) = get(tx, &offer.share_id)? {
        return Ok(if known.peer_id == peer_id {
            ack("duplicate")
        } else {
            error("conflict")
        });
    }
    let held: i64 = tx.query_row(
        "SELECT count(*) FROM peer_shares WHERE peer_id=?1 AND state<>'removed'",
        [peer_id],
        |r| r.get(0),
    )?;
    if held >= MAX_PER_PEER {
        return Ok(error("too_many_shares"));
    }
    tx.execute(
        "INSERT INTO peer_shares (share_id,peer_id,label,local_repo,inbound,outbound,
           remote_inbound,remote_outbound,revision,state,root_commit)
         VALUES (?1,?2,?3,NULL,0,0,?4,?5,?6,'offered_in',?7)",
        params![
            offer.share_id,
            peer_id,
            offer.label,
            offer.inbound,
            offer.outbound,
            offer.revision,
            offer.root_commit
        ],
    )?;
    Ok(ack("accepted"))
}

fn flagged(tx: &Connection, peer_id: &str, flags: &Flags, accept: bool) -> anyhow::Result<Value> {
    if !revision_ok(flags.revision) {
        return Ok(error("bad_frame"));
    }
    let Some(share) = owned(tx, peer_id, &flags.share_id)? else {
        return Ok(error("unknown_share"));
    };
    // An accept makes our offer active; an update only changes a share both sides know.
    let fits = matches!(
        (share.state.as_str(), accept),
        ("offered_out", true) | ("active", _) | ("offered_in", false)
    );
    if !fits {
        return Ok(error("wrong_state"));
    }
    // Both owners bump the one revision, so two changes can cross: an equal revision still
    // carries news when the flags differ, an older one never does.
    let known = (flags.inbound, flags.outbound) == (share.remote_inbound, share.remote_outbound);
    let news = flags.revision > share.revision || (flags.revision == share.revision && !known);
    let first_accept = accept && share.state == "offered_out";
    if !(news || first_accept) {
        return Ok(ack("duplicate"));
    }
    let state = if accept {
        "active"
    } else {
        share.state.as_str()
    };
    tx.execute(
        "UPDATE peer_shares SET remote_inbound=?2, remote_outbound=?3, revision=?4, state=?5
         WHERE share_id=?1",
        params![
            share.share_id,
            flags.inbound,
            flags.outbound,
            flags.revision,
            state
        ],
    )?;
    Ok(ack("accepted"))
}

fn removed(tx: &Connection, peer_id: &str, remove: &Remove) -> anyhow::Result<Value> {
    if !revision_ok(remove.revision) {
        return Ok(error("bad_frame"));
    }
    let Some(share) = owned(tx, peer_id, &remove.share_id)? else {
        // Never known, or not theirs: nothing to remove, and nothing to learn from the answer.
        return Ok(ack("duplicate"));
    };
    if share.state == "removed" {
        return Ok(ack("duplicate"));
    }
    close(tx, &share, remove.revision.max(share.revision))?;
    Ok(ack("accepted"))
}
