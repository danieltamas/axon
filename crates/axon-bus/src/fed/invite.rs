//! The invitation (docs/P2P-SPEC.md §4): the `axon1:` blob a joiner pastes and the
//! `peer_invites` row behind it. The row holds only the secret's hash.

use anyhow::Context;
use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{identity, now_ms, random_id};
use crate::session::{base64url, random_secret, sha256_hex, BASE64URL_ALPHABET};
use crate::store;

pub const LIFETIME_MS: i64 = 600_000;
/// The fifth wrong secret kills an invite.
pub const MAX_ATTEMPTS: i64 = 5;
const PREFIX: &str = "axon1:";
/// Longer than any invite we issue; bounds what a pasted blob can make us decode.
const MAX_BLOB_CHARS: usize = 4096;
const MAX_ADDRS: usize = 16;
pub const SECRET_CHARS: usize = 43;

#[derive(Serialize, Deserialize)]
struct Blob {
    v: i64,
    invite_id: String,
    node_id: String,
    relay_url: Option<String>,
    direct_addrs: Vec<String>,
    secret: String,
    expires_at: i64,
    /// What the inviter suggests to be called; the joiner's own label is what is stored.
    label: String,
}

/// A blob that is malformed, of another version, or names an unusable address. The caller
/// says only `invalid_invite`, whatever the reason.
#[derive(Debug)]
pub struct BadInvite;

/// What a joiner needs from a blob that parsed.
pub struct Joinable {
    pub invite_id: String,
    pub secret: String,
    pub addr: EndpointAddr,
}

pub struct Issued {
    pub invite_id: String,
    pub invite: String,
    pub expires_at: i64,
}

impl Issued {
    pub fn to_json(&self) -> Value {
        json!({"invite_id": self.invite_id, "invite": self.invite, "expires_at": self.expires_at})
    }
}

/// A new invite for the node at `addr`. Only one is open at a time: the old one is
/// cancelled in the same transaction.
pub fn issue(conn: &mut Connection, addr: &EndpointAddr) -> anyhow::Result<Issued> {
    let now = now_ms();
    let (invite_id, secret) = (random_id()?, random_secret()?);
    let expires_at = now + LIFETIME_MS;
    let tx = store::write_tx(conn)?;
    tx.execute(
        "UPDATE peer_invites SET cancelled_at=?1 WHERE cancelled_at IS NULL AND consumed_by IS NULL",
        [now],
    )?;
    tx.execute(
        "INSERT INTO peer_invites (invite_id, secret_hash, expires_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![invite_id, sha256_hex(secret.as_bytes()), expires_at],
    )?;
    tx.commit()?;
    let blob = Blob {
        v: 1,
        invite_id: invite_id.clone(),
        node_id: addr.id.to_string(),
        relay_url: addr.relay_urls().next().map(ToString::to_string),
        direct_addrs: addr.ip_addrs().map(ToString::to_string).collect(),
        secret,
        expires_at,
        label: format!("axon-{}", &identity::fingerprint(&addr.id)[..4]),
    };
    let json = serde_json::to_vec(&blob).context("encode the invite")?;
    Ok(Issued {
        invite_id,
        invite: format!("{PREFIX}{}", base64url(&json)),
        expires_at,
    })
}

/// Cancel an invite that has not been used. False when there is no such open invite.
pub fn cancel(conn: &Connection, invite_id: &str) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "UPDATE peer_invites SET cancelled_at=COALESCE(cancelled_at, ?1)
         WHERE invite_id=?2 AND consumed_by IS NULL",
        rusqlite::params![now_ms(), invite_id],
    )?;
    Ok(changed > 0)
}

/// Invites that could still be redeemed, as `GET /api/fed` lists them.
pub fn open(conn: &Connection) -> rusqlite::Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT invite_id, expires_at FROM peer_invites
         WHERE cancelled_at IS NULL AND consumed_by IS NULL AND attempts < ?1 AND expires_at > ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![MAX_ATTEMPTS, now_ms()], |r| {
        Ok(json!({"invite_id": r.get::<_, String>(0)?, "expires_at": r.get::<_, i64>(1)?}))
    })?;
    rows.collect()
}

/// The stored invite: `(secret_hash, expires_at, attempts, consumed_by, cancelled_at)`.
pub type Stored = (String, i64, i64, Option<String>, Option<i64>);

pub fn stored(conn: &Connection, invite_id: &str) -> rusqlite::Result<Option<Stored>> {
    conn.query_row(
        "SELECT secret_hash, expires_at, attempts, consumed_by, cancelled_at
         FROM peer_invites WHERE invite_id=?1",
        [invite_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .optional()
}

/// Compare in constant time, over hashes of equal length.
pub fn secret_matches(stored_hash: &str, presented: &str) -> bool {
    let presented = sha256_hex(presented.as_bytes());
    stored_hash.len() == presented.len()
        && stored_hash
            .bytes()
            .zip(presented.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

/// Only base64url characters (hex ids are a subset), `len` of them: the shape of every id
/// and secret we issue.
pub fn is_token(text: &str, len: std::ops::RangeInclusive<usize>) -> bool {
    len.contains(&text.len()) && text.bytes().all(|c| BASE64URL_ALPHABET.contains(&c))
}

fn base64url_decode(text: &str) -> Option<Vec<u8>> {
    let (mut out, mut bits, mut held) = (Vec::new(), 0u32, 0u32);
    for c in text.bytes() {
        let digit = BASE64URL_ALPHABET.iter().position(|a| *a == c)? as u32;
        bits = (bits << 6) | digit;
        held += 6;
        if held >= 8 {
            held -= 8;
            out.push((bits >> held) as u8);
            bits &= (1 << held) - 1;
        }
    }
    Some(out)
}

pub fn parse(text: &str) -> Result<Joinable, BadInvite> {
    let encoded = text.strip_prefix(PREFIX).ok_or(BadInvite)?;
    if encoded.len() > MAX_BLOB_CHARS {
        return Err(BadInvite);
    }
    let blob: Blob = serde_json::from_slice(&base64url_decode(encoded).ok_or(BadInvite)?)
        .map_err(|_| BadInvite)?;
    if blob.v != 1
        || !is_token(&blob.invite_id, 1..=64)
        || !is_token(&blob.secret, SECRET_CHARS..=SECRET_CHARS)
        || blob.direct_addrs.len() > MAX_ADDRS
    {
        return Err(BadInvite);
    }
    let node: EndpointId = blob.node_id.parse().map_err(|_| BadInvite)?;
    let mut addrs = Vec::new();
    for addr in &blob.direct_addrs {
        addrs.push(TransportAddr::Ip(addr.parse().map_err(|_| BadInvite)?));
    }
    if let Some(url) = &blob.relay_url {
        // The settings page insists on https for a relay the owner picks; an invite from
        // someone else gets no weaker rule.
        if !url.starts_with("https://") {
            return Err(BadInvite);
        }
        addrs.push(TransportAddr::Relay(
            url.parse::<RelayUrl>().map_err(|_| BadInvite)?,
        ));
    }
    Ok(Joinable {
        invite_id: blob.invite_id,
        secret: blob.secret,
        addr: EndpointAddr::from_parts(node, addrs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn hub() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = store::init(&dir.path().join("axon.db")).unwrap();
        (dir, conn)
    }

    fn node() -> EndpointAddr {
        EndpointAddr::new(SecretKey::from_bytes(&[7; 32]).public())
            .with_ip_addr("127.0.0.1:4000".parse().unwrap())
    }

    #[test]
    fn an_issued_invite_parses_and_only_its_hash_is_stored() {
        let (_dir, mut conn) = hub();
        let issued = issue(&mut conn, &node()).unwrap();
        let joinable = parse(&issued.invite).unwrap();
        assert_eq!(joinable.invite_id, issued.invite_id);
        assert_eq!(joinable.addr.id, node().id);
        let (hash, ..) = stored(&conn, &issued.invite_id).unwrap().unwrap();
        assert_ne!(hash, joinable.secret);
        assert!(secret_matches(&hash, &joinable.secret));
        assert!(!secret_matches(&hash, &"A".repeat(SECRET_CHARS)));
    }

    #[test]
    fn a_new_invite_cancels_the_open_one_and_cancel_ends_it() {
        let (_dir, mut conn) = hub();
        let first = issue(&mut conn, &node()).unwrap();
        let second = issue(&mut conn, &node()).unwrap();
        assert_eq!(open(&conn).unwrap().len(), 1);
        assert!(cancel(&conn, &second.invite_id).unwrap());
        assert!(open(&conn).unwrap().is_empty());
        assert!(!cancel(&conn, "missing").unwrap());
        assert!(stored(&conn, &first.invite_id)
            .unwrap()
            .unwrap()
            .4
            .is_some());
    }

    #[test]
    fn malformed_blobs_are_refused() {
        let (_dir, mut conn) = hub();
        let good = issue(&mut conn, &node()).unwrap().invite;
        for bad in [
            "",
            "axon1:",
            "axon2:abcd",
            "axon1:!!!!",
            &good[..good.len() - 8],
            &format!("{good}{}", "A".repeat(MAX_BLOB_CHARS)),
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_relay_url_that_is_not_https_makes_the_invite_unusable() {
        let blob = |relay: &str| {
            let json = format!(
                r#"{{"v":1,"node_id":"{}","invite_id":"i1","secret":"{}","direct_addrs":[],"relay_url":"{relay}","expires_at":1,"label":"x"}}"#,
                node().id,
                "A".repeat(SECRET_CHARS)
            );
            format!("{PREFIX}{}", crate::session::base64url(json.as_bytes()))
        };
        assert!(parse(&blob("https://relay.example.com")).is_ok());
        for relay in [
            "http://169.254.169.254",
            "ftp://relay.example.com",
            "relay.example.com",
        ] {
            assert!(parse(&blob(relay)).is_err(), "{relay}");
        }
    }
}
