//! The inviter's side of `axon/pair/1`: check the invite and admit the first valid joiner.

use std::path::PathBuf;
use std::sync::Arc;

use iroh::EndpointAddr;
use rusqlite::{params, Connection, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{history_generation, insert_pending, label_ok, pending_by_node, MAX_GENERATION};
use crate::fed::now_ms;
use crate::fed::service::{Link, PairHandler};
use crate::fed::{identity, invite};
use crate::store;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinRequest {
    v: i64,
    invite_id: String,
    secret: String,
    label: String,
    /// The joiner's own next generation for this node; the inviter takes the larger so
    /// both sides end on one number.
    generation: i64,
}

impl JoinRequest {
    fn well_formed(&self) -> bool {
        self.v == 1
            && invite::is_token(&self.invite_id, 1..=64)
            && invite::is_token(&self.secret, invite::SECRET_CHARS..=invite::SECRET_CHARS)
            && label_ok(&self.label)
            && (1..=MAX_GENERATION).contains(&self.generation)
    }
}

enum Admission {
    Admitted {
        generation: i64,
    },
    /// Wrong, expired, consumed, cancelled or exhausted: one answer for all of them.
    Refused,
}

impl Admission {
    fn reply(&self) -> Value {
        match self {
            Self::Admitted { generation } => json!({"ok": true, "generation": generation}),
            Self::Refused => json!({"ok": false, "error": "invalid_invite"}),
        }
    }
}

/// What the inviter calls a joiner until its owner renames it. The name the joiner chose
/// for us is never used (it could pose as someone else on the confirmation screen), and one
/// derived from the node's own fingerprint cannot clash with another live peer's.
fn neutral_label(joiner: &str) -> String {
    let print = joiner
        .parse::<iroh::EndpointId>()
        .map_or_else(|_| "peer".to_owned(), |node| identity::fingerprint(&node));
    format!("axon-{}", print.replace(' ', ""))
}

/// Check the invite and, for the first valid joiner, create its pending row and consume the
/// invite: one transaction, committed before the answer leaves (`synchronous=FULL`). A wrong
/// secret commits its attempt count, so attempts survive a restart.
fn admit(conn: &mut Connection, joiner: &str, request: &JoinRequest) -> anyhow::Result<Admission> {
    // An unknown invite costs the dialer nothing to send; it must not cost a write lock.
    if invite::stored(conn, &request.invite_id)?.is_none() {
        return Ok(Admission::Refused);
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    let tx = store::write_tx(conn)?;
    let admission = admit_in(&tx, now_ms(), joiner, request)?;
    tx.commit()?;
    Ok(admission)
}

fn admit_in(
    tx: &Transaction,
    now: i64,
    joiner: &str,
    request: &JoinRequest,
) -> anyhow::Result<Admission> {
    let Some((hash, expires_at, attempts, consumed_by, cancelled_at)) =
        invite::stored(tx, &request.invite_id)?
    else {
        return Ok(Admission::Refused);
    };
    if cancelled_at.is_some() || now >= expires_at || attempts >= invite::MAX_ATTEMPTS {
        return Ok(Admission::Refused);
    }
    if consumed_by.as_deref().is_some_and(|by| by != joiner) {
        return Ok(Admission::Refused);
    }
    if !invite::secret_matches(&hash, &request.secret) {
        tx.execute(
            "UPDATE peer_invites SET attempts=attempts+1 WHERE invite_id=?1",
            [&request.invite_id],
        )?;
        return Ok(Admission::Refused);
    }
    if consumed_by.is_some() {
        // The same joiner asking again gets the pairing it already has, if still pending.
        return Ok(match pending_by_node(tx, joiner)? {
            Some(peer) => Admission::Admitted {
                generation: peer.generation,
            },
            None => Admission::Refused,
        });
    }
    let live: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM peers WHERE node_id=?1 AND state<>'removed')",
        [joiner],
        |r| r.get(0),
    )?;
    if live {
        return Ok(Admission::Refused);
    }
    let generation = (history_generation(tx, joiner)? + 1).max(request.generation);
    insert_pending(tx, joiner, &neutral_label(joiner), generation)?;
    tx.execute(
        "UPDATE peer_invites SET consumed_by=?1 WHERE invite_id=?2",
        params![joiner, request.invite_id],
    )?;
    Ok(Admission::Admitted { generation })
}

pub fn pair_handler(db: PathBuf, link: Link) -> PairHandler {
    Arc::new(move |remote: EndpointAddr, frame: Value| {
        let (db, link) = (db.clone(), link.clone());
        Box::pin(async move {
            let Some(request) = serde_json::from_value::<JoinRequest>(frame)
                .ok()
                .filter(JoinRequest::well_formed)
            else {
                return Admission::Refused.reply();
            };
            let joiner = remote.id.to_string();
            let admitted = tokio::task::spawn_blocking(move || {
                admit(&mut crate::fed::open_durable(&db)?, &joiner, &request)
            })
            .await;
            match admitted {
                Ok(Ok(admission)) => {
                    if matches!(admission, Admission::Admitted { .. }) {
                        link.add_addr(remote);
                    }
                    admission.reply()
                }
                Ok(Err(err)) => {
                    eprintln!("axon-bus: pairing could not admit a joiner: {err:#}");
                    json!({"ok": false, "error": "unavailable"})
                }
                Err(_) => json!({"ok": false, "error": "unavailable"}),
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fed::invite::Joinable;
    use iroh::SecretKey;

    struct Fixture {
        _dir: tempfile::TempDir,
        conn: Connection,
        invite: Joinable,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let mut conn = store::init(&dir.path().join("axon.db")).unwrap();
        let node = SecretKey::from_bytes(&[1; 32]).public();
        let issued = invite::issue(&mut conn, &iroh::EndpointAddr::new(node)).unwrap();
        let invite = invite::parse(&issued.invite).unwrap();
        Fixture {
            _dir: dir,
            conn,
            invite,
        }
    }

    fn joiner(seed: u8) -> String {
        SecretKey::from_bytes(&[seed; 32]).public().to_string()
    }

    fn request(fx: &Fixture, secret: &str, label: &str) -> JoinRequest {
        JoinRequest {
            v: 1,
            invite_id: fx.invite.invite_id.clone(),
            secret: secret.to_owned(),
            label: label.to_owned(),
            generation: 1,
        }
    }

    fn admitted(admission: Admission) -> bool {
        matches!(admission, Admission::Admitted { .. })
    }

    fn attempts(fx: &Fixture) -> i64 {
        invite::stored(&fx.conn, &fx.invite.invite_id)
            .unwrap()
            .unwrap()
            .2
    }

    #[test]
    fn the_fifth_wrong_secret_kills_the_invite_for_everyone() {
        let mut fx = fixture();
        let wrong = "A".repeat(invite::SECRET_CHARS);
        for _ in 0..5 {
            let bad = request(&fx, &wrong, "alice");
            assert!(!admitted(admit(&mut fx.conn, &joiner(2), &bad).unwrap()));
        }
        assert_eq!(attempts(&fx), 5);
        let good = request(&fx, &fx.invite.secret, "alice");
        assert!(!admitted(admit(&mut fx.conn, &joiner(2), &good).unwrap()));
    }

    #[test]
    fn four_wrong_secrets_leave_one_valid_join_and_a_retry_by_the_same_node() {
        let mut fx = fixture();
        let wrong = "A".repeat(invite::SECRET_CHARS);
        for _ in 0..4 {
            let bad = request(&fx, &wrong, "alice");
            admit(&mut fx.conn, &joiner(2), &bad).unwrap();
        }
        let good = request(&fx, &fx.invite.secret, "alice");
        assert!(admitted(admit(&mut fx.conn, &joiner(2), &good).unwrap()));
        assert!(admitted(admit(&mut fx.conn, &joiner(2), &good).unwrap()));
        let rows: i64 = fx
            .conn
            .query_row("SELECT count(*) FROM peers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        assert!(!admitted(admit(&mut fx.conn, &joiner(3), &good).unwrap()));
    }

    #[test]
    fn the_joiners_chosen_label_is_never_stored_the_inviter_assigns_a_neutral_one() {
        let mut fx = fixture();
        let good = request(&fx, &fx.invite.secret, "ceo-of-your-bank");
        assert!(admitted(admit(&mut fx.conn, &joiner(2), &good).unwrap()));
        let label: String = fx
            .conn
            .query_row("SELECT label FROM peers", [], |r| r.get(0))
            .unwrap();
        assert!(label.starts_with("axon-") && label.len() == 21, "{label}");
        assert!(label_ok(&label));
    }

    #[test]
    fn an_expired_or_cancelled_invite_is_refused_even_with_the_right_secret() {
        let mut fx = fixture();
        let good = request(&fx, &fx.invite.secret, "alice");
        let late = now_ms() + invite::LIFETIME_MS;
        let tx = store::write_tx(&mut fx.conn).unwrap();
        let expired = admit_in(&tx, late, &joiner(2), &good).unwrap();
        assert!(matches!(expired, Admission::Refused));
        drop(tx);
        invite::cancel(&fx.conn, &fx.invite.invite_id).unwrap();
        assert!(!admitted(admit(&mut fx.conn, &joiner(2), &good).unwrap()));
    }

    #[test]
    fn the_agreed_generation_is_the_larger_of_both_sides_histories() {
        let mut fx = fixture();
        fx.conn
            .execute(
                "INSERT INTO peers (peer_id,node_id,label,generation,state,paired_at,removed_at)
                 VALUES ('old',?1,'old',4,'removed',1,2)",
                [joiner(2)],
            )
            .unwrap();
        let mut good = request(&fx, &fx.invite.secret, "alice");
        good.generation = 2;
        match admit(&mut fx.conn, &joiner(2), &good).unwrap() {
            Admission::Admitted { generation } => assert_eq!(generation, 5),
            _ => panic!("the invite is valid"),
        }
    }
}
