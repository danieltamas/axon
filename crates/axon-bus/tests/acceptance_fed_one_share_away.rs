//! Frozen acceptance contract: P2P-SPEC §§6–7, one share per repo/peer and away sessions.
//! Expectations come from the spec; services, Git repositories and SQL fixtures use Bus sandboxes.
mod common;

use common::fed::*;
use common::{now_ms, parse_json, Bus, Response, Server};
use rusqlite::{params, types::Value as SqlValue, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn paired() -> Pair {
    Pair::paired(Duration::from_secs(80))
}

fn clone_repo(bus: &Bus, source: &Path, name: &str) -> PathBuf {
    let path = bus.root.join(name);
    git(
        bus,
        &bus.root,
        &[
            "clone",
            "--no-hardlinks",
            source.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    // Equal roots with different tips must still match the project's identity hint.
    git(
        bus,
        &path,
        &["commit", "--allow-empty", "-m", "local continuation"],
    );
    common::canonical(&path)
}

fn offer(bus: &Bus, server: &Server, peer_id: &str, repo: &Path) -> String {
    api(
        bus,
        server,
        "POST",
        &format!("/api/fed/peers/{peer_id}/shares"),
        json!({"local_repo":repo,"label":"project","inbound":true,"outbound":true}),
    );
    db(bus).query_row(
        "SELECT share_id FROM peer_shares WHERE peer_id=?1 AND local_repo=?2 AND state='offered_out'",
        params![peer_id, repo.to_str().unwrap()], |row| row.get(0),
    ).unwrap()
}

fn incoming(bus: &Bus, share: &str) {
    eventually(bus, "share offer arrives unmapped", || {
        db(bus).query_row(
            "SELECT count(*) FROM peer_shares WHERE share_id=?1 AND state='offered_in' AND local_repo IS NULL",
            [share], |row| row.get::<_, i64>(0),
        ).unwrap() == 1
    });
}

fn shares(bus: &Bus) -> Vec<Vec<SqlValue>> {
    db(bus)
        .prepare(
            "SELECT share_id,peer_id,label,local_repo,inbound,outbound,remote_inbound,
         remote_outbound,revision,state,root_commit FROM peer_shares ORDER BY share_id",
        )
        .unwrap()
        .query_map([], |row| (0..11).map(|i| row.get(i)).collect())
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn conflict(response: Response, error: &str) {
    assert_eq!(
        response.status,
        409,
        "expected 409 {error}, got {} {}",
        response.status,
        String::from_utf8_lossy(&response.body)
    );
    assert_eq!(parse_json(&response.body), json!({"error":error}));
}

fn listed_share(bus: &Bus, server: &Server, peer_id: &str, share: &str) -> Value {
    let current = peer(bus, server, peer_id);
    current["shares"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["share_id"] == share)
        .unwrap_or_else(|| panic!("share {share} missing from peers list: {current}"))
        .clone()
}

fn no_suggestion(share: &Value) {
    assert!(
        share.get("suggested_repo").is_none_or(Value::is_null),
        "non-offered_in share must have no suggestion: {share}"
    );
}

fn duplicate_offer(active: bool) {
    let pair = paired();
    let share = if active {
        pair.share(true, true, true, true)
    } else {
        let share = offer(&pair.a, &pair.sa, &pair.pa, &pair.ra);
        incoming(&pair.b, &share);
        share
    };
    let before = shares(&pair.a);
    let response = request(
        &pair.a,
        &pair.sa,
        "POST",
        &format!("/api/fed/peers/{}/shares", pair.pa),
        json!({"local_repo":pair.ra,"label":"another label","inbound":false,"outbound":false}),
    );
    assert_eq!(
        shares(&pair.a),
        before,
        "duplicate offer must not insert or mutate a share"
    );
    conflict(response, "already_shared");
    assert_eq!(
        text(
            &pair.a,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &share
        ),
        if active { "active" } else { "offered_out" }
    );
}

#[test]
fn offer_rejects_repo_in_an_active_share() {
    duplicate_offer(true);
}

#[test]
fn offer_rejects_repo_in_a_pending_outgoing_share() {
    duplicate_offer(false);
}

#[test]
fn offer_back_with_same_root_is_rejected_without_a_new_share() {
    let pair = paired();
    let local = clone_repo(&pair.b, &pair.ra, "same-project");
    pair.b.register_at("matching-agent", "codex", None, &local);
    let share = offer(&pair.a, &pair.sa, &pair.pa, &pair.ra);
    incoming(&pair.b, &share);
    let before = shares(&pair.b);
    let response = request(
        &pair.b,
        &pair.sb,
        "POST",
        &format!("/api/fed/peers/{}/shares", pair.pb),
        json!({"local_repo":local,"label":"offer back","inbound":true,"outbound":true}),
    );
    assert_eq!(
        shares(&pair.b),
        before,
        "offer-back rejection must preserve all share rows"
    );
    conflict(response, "offered_to_you");
    assert_eq!(
        text(
            &pair.b,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &share
        ),
        "offered_in"
    );
}

fn duplicate_accept(active: bool) {
    let pair = paired();
    if active {
        pair.share(true, true, true, true);
    } else {
        let occupied = offer(&pair.b, &pair.sb, &pair.pb, &pair.rb);
        incoming(&pair.a, &occupied);
    }
    let another = repo(&pair.a, "another-project");
    let share = offer(&pair.a, &pair.sa, &pair.pa, &another);
    incoming(&pair.b, &share);
    let before = shares(&pair.b);
    let response = request(
        &pair.b,
        &pair.sb,
        "POST",
        &format!("/api/fed/shares/{share}/accept"),
        json!({"local_repo":pair.rb,"inbound":true,"outbound":true}),
    );
    assert_eq!(
        shares(&pair.b),
        before,
        "rejected accept must leave the incoming offer unmapped and unchanged"
    );
    conflict(response, "already_shared");
    assert_eq!(
        text(
            &pair.b,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &share
        ),
        "offered_in"
    );
    assert_eq!(
        text(
            &pair.a,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &share
        ),
        "offered_out"
    );
}

#[test]
fn accept_rejects_repo_in_an_active_share_and_preserves_offer() {
    duplicate_accept(true);
}

#[test]
fn accept_rejects_repo_in_a_pending_outgoing_share_and_preserves_offer() {
    duplicate_accept(false);
}

#[test]
fn removed_share_does_not_prevent_a_new_offer() {
    let pair = paired();
    let previous = pair.share(true, true, true, true);
    api(
        &pair.a,
        &pair.sa,
        "DELETE",
        &format!("/api/fed/shares/{previous}"),
        json!({}),
    );
    eventually(&pair.b, "unshare reaches peer", || {
        text(
            &pair.b,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &previous,
        ) == "removed"
    });
    let next = offer(&pair.a, &pair.sa, &pair.pa, &pair.ra);
    assert_ne!(next, previous);
    incoming(&pair.b, &next);
    assert_eq!(
        count(
            &pair.a,
            "SELECT count(*) FROM peer_shares WHERE state<>'removed'"
        ),
        1
    );
}

fn matching_suggestion(status: &str) {
    let pair = paired();
    let local = clone_repo(&pair.b, &pair.ra, "matching-project");
    pair.b.register_at("matching-agent", "codex", None, &local);
    if status == "active" {
        let mut prompt = pair.b.fixture("codex", "UserPromptSubmit");
        prompt["session_id"] = json!("matching-agent");
        prompt["cwd"] = json!(local);
        pair.b.hook("codex", "UserPromptSubmit", &prompt);
    }
    assert_eq!(pair.b.agent("matching-agent")["status"], status);
    let share = offer(&pair.a, &pair.sa, &pair.pa, &pair.ra);
    incoming(&pair.b, &share);
    let listed = listed_share(&pair.b, &pair.sb, &pair.pb, &share);
    assert_eq!(
        listed.get("suggested_repo"),
        Some(&json!(local)),
        "offered_in must suggest the matching repo with a {status} registered agent: {listed}"
    );
    assert!(
        listed["local_repo"].is_null(),
        "a suggestion must not accept or map the offer"
    );
    no_suggestion(&listed_share(&pair.a, &pair.sa, &pair.pa, &share));
    // A different project remains a valid explicit choice; the root is only a hint.
    api(
        &pair.b,
        &pair.sb,
        "POST",
        &format!("/api/fed/shares/{share}/accept"),
        json!({"local_repo":pair.rb,"inbound":true,"outbound":true}),
    );
    for (bus, server, peer_id) in [(&pair.a, &pair.sa, &pair.pa), (&pair.b, &pair.sb, &pair.pb)] {
        eventually(bus, "accepted share is active", || {
            text(
                bus,
                "SELECT state FROM peer_shares WHERE share_id=?1",
                &share,
            ) == "active"
        });
        no_suggestion(&listed_share(bus, server, peer_id, &share));
    }
    assert_eq!(
        text(
            &pair.b,
            "SELECT local_repo FROM peer_shares WHERE share_id=?1",
            &share
        ),
        pair.rb.to_str().unwrap()
    );
}

#[test]
fn peers_suggest_matching_repo_with_idle_registered_agent() {
    matching_suggestion("idle");
}

#[test]
fn peers_suggest_matching_repo_with_active_registered_agent() {
    matching_suggestion("active");
}

fn missing_suggestion(matching_agent: Option<&str>) {
    let pair = paired();
    if let Some(state) = matching_agent {
        let local = clone_repo(&pair.b, &pair.ra, "matching-but-ineligible");
        if state == "closed" {
            pair.b.register_at("closed-agent", "codex", None, &local);
            pair.b
                .hook("codex", "SessionEnd", &json!({"session_id":"closed-agent"}));
            assert_eq!(pair.b.agent("closed-agent")["status"], "closed");
        }
    }
    let share = offer(&pair.a, &pair.sa, &pair.pa, &pair.ra);
    incoming(&pair.b, &share);
    let listed = listed_share(&pair.b, &pair.sb, &pair.pb, &share);
    assert_eq!(
        listed.get("suggested_repo"),
        Some(&Value::Null),
        "offered_in must carry explicit null when no eligible repo matches: {listed}"
    );
}

#[test]
fn peers_suggest_null_when_live_repos_have_different_roots() {
    missing_suggestion(None);
}

#[test]
fn peers_suggest_null_for_matching_repo_without_registered_agents() {
    missing_suggestion(Some("unregistered"));
}

#[test]
fn peers_suggest_null_for_matching_repo_with_only_closed_agents() {
    missing_suggestion(Some("closed"));
}

#[test]
fn peers_suggest_null_for_matching_repo_already_shared_with_peer() {
    let pair = paired();
    pair.share(true, true, true, true);
    let another = clone_repo(&pair.a, &pair.rb, "second-offered-project");
    let share = offer(&pair.a, &pair.sa, &pair.pa, &another);
    incoming(&pair.b, &share);
    let listed = listed_share(&pair.b, &pair.sb, &pair.pb, &share);
    assert_eq!(
        listed.get("suggested_repo"),
        Some(&Value::Null),
        "the matching repo is occupied by another live share with this peer: {listed}"
    );
}

fn aged_remote(age_ms: i64) -> (Pair, String, String) {
    let mut pair = paired();
    let share = pair.share(true, true, true, true);
    let target = pair.target(true, "agent-b");
    let session = target.rsplit('/').next().unwrap();
    // Stop both sandbox services so an in-flight discovery cannot refresh the timestamp.
    for server in [&mut pair.sa, &mut pair.sb] {
        server.process.0.kill().unwrap();
        server.process.0.wait().unwrap();
    }
    assert_eq!(
        pair.a
            .db()
            .execute(
                "UPDATE fed_remote_sessions SET seen_at=?1 WHERE peer_id=?2 AND session=?3",
                params![now_ms() - age_ms, pair.pa, session],
            )
            .unwrap(),
        1,
        "age a real discovered session, not a synthetic cache row"
    );
    // Restart only the sender: the recipient really is offline throughout the assertions.
    pair.sa = Server::start(&pair.a, true);
    (pair, share, target)
}

fn availability(pair: &Pair, target: &str) -> Option<String> {
    db(&pair.a)
        .query_row(
            "SELECT availability FROM fed_remote_sessions WHERE peer_id=?1 AND session=?2",
            params![pair.pa, target.rsplit('/').next().unwrap()],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

fn await_away(pair: &Pair, target: &str) {
    eventually(&pair.a, "unconfirmed session becomes away", || {
        let state = availability(pair, target);
        assert!(
            state.is_some(),
            "session unconfirmed for >60 s must remain addressable until 24 h; row was deleted"
        );
        state.as_deref() == Some("away")
    });
}

#[test]
fn session_unconfirmed_for_over_sixty_seconds_is_away_and_listed() {
    let (pair, _, target) = aged_remote(61_000);
    await_away(&pair, &target);
    let listing = String::from_utf8(pair.a.ok(&["peers", "--agent", "agent-a"]).stdout).unwrap();
    let session = target.rsplit('/').next().unwrap();
    let label = format!("codex-{}", &session[..4]);
    // §7 specifies the target, label and availability, not any trailing explanatory prose.
    assert!(
        listing
            .lines()
            .any(|line| line.split_whitespace().take(3).eq([
                target.as_str(),
                label.as_str(),
                "away"
            ])),
        "peers must keep the away session visible:\n{listing}"
    );
}

#[test]
fn send_to_unconfirmed_session_queues_before_the_twenty_four_hour_limit() {
    let (pair, share, target) = aged_remote(24 * 60 * 60 * 1000 - 60_000);
    // Assert send independently of the listing test, including almost the entire lifetime.
    let message = queued(
        &pair.a,
        "agent-a",
        &target,
        "sync",
        "deliver when the peer returns",
    );
    assert_eq!(
        text(
            &pair.a,
            "SELECT state FROM fed_outbox WHERE message_id=?1",
            &message
        ),
        "queued"
    );
    assert_eq!(
        text(
            &pair.a,
            "SELECT share_id FROM fed_outbox WHERE message_id=?1",
            &message
        ),
        share
    );
    assert_eq!(count(&pair.b, "SELECT count(*) FROM fed_inbox"), 0);
    await_away(&pair, &target);
}

#[test]
fn session_unconfirmed_for_over_twenty_four_hours_is_dropped() {
    let (pair, _, target) = aged_remote(24 * 60 * 60 * 1000 + 60_000);
    eventually(
        &pair.a,
        "session expires after one message lifetime",
        || availability(&pair, &target).is_none(),
    );
    let listing = String::from_utf8(pair.a.ok(&["peers", "--agent", "agent-a"]).stdout).unwrap();
    assert!(
        !listing.contains(&target),
        "expired session must disappear from peers: {listing}"
    );
    refused(
        &pair.a,
        "agent-a",
        &target,
        "sync",
        "expired addressee",
        "unknown_session",
    );
}

#[test]
fn successful_discovery_drops_a_session_the_peer_no_longer_lists() {
    let pair = paired();
    let share = pair.share(true, true, true, true);
    let target = pair.target(true, "agent-b");
    pair.b
        .hook("codex", "SessionEnd", &json!({"session_id":"agent-b"}));
    api(
        &pair.b,
        &pair.sb,
        "PUT",
        &format!("/api/fed/shares/{share}"),
        json!({"inbound":true,"outbound":true}),
    );
    eventually(
        &pair.a,
        "successful discovery removes the closed session instead of retaining it as away",
        || availability(&pair, &target).is_none(),
    );
    let listing = String::from_utf8(pair.a.ok(&["peers", "--agent", "agent-a"]).stdout).unwrap();
    assert!(
        !listing.contains(&target),
        "explicitly withdrawn session must not remain away: {listing}"
    );
}
