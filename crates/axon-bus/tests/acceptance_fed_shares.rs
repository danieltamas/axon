//! P2P-SPEC §§5–7: share mapping, direction, membership and discovery; A07–A10/A21/A22.
mod common;
use common::fed::*;
use serde_json::json;
use std::time::Duration;

fn shared() -> (Pair, String, String) {
    let pair = Pair::paired(Duration::from_secs(70));
    let share = pair.share(true, true, true, true);
    let target = pair.target(true, "agent-b");
    (pair, share, target)
}
fn flags(pair: &Pair, from_a: bool, share: &str, inbound: bool, outbound: bool) {
    let (bus, server) = if from_a {
        (&pair.a, &pair.sa)
    } else {
        (&pair.b, &pair.sb)
    };
    api(
        bus,
        server,
        "PUT",
        &format!("/api/fed/shares/{share}"),
        json!({"inbound":inbound,"outbound":outbound}),
    );
}

#[test]
fn no_remote_repo_authority_until_both_owners_explicitly_map_the_offer() {
    let pair = Pair::paired(Duration::from_secs(50));
    api(
        &pair.a,
        &pair.sa,
        "POST",
        &format!("/api/fed/peers/{}/shares", pair.pa),
        json!({"local_repo":pair.ra,"label":"project","inbound":true,"outbound":true}),
    );
    let share = text(
        &pair.a,
        "SELECT share_id FROM peer_shares WHERE peer_id=?1",
        &pair.pa,
    );
    eventually(&pair.b, "unmapped offer", || {
        count(&pair.b, "SELECT count(*) FROM peer_shares") == 1
    });
    assert_eq!(
        count(
            &pair.b,
            "SELECT count(*) FROM peer_shares WHERE local_repo IS NULL AND state='offered_in'"
        ),
        1
    );
    assert_eq!(
        count(&pair.a, "SELECT count(*) FROM fed_remote_sessions"),
        0
    );
    assert_eq!(
        count(&pair.b, "SELECT count(*) FROM fed_remote_sessions"),
        0
    );
    api(
        &pair.b,
        &pair.sb,
        "POST",
        &format!("/api/fed/shares/{share}/accept"),
        json!({"local_repo":pair.rb,"inbound":true,"outbound":true}),
    );
    eventually(&pair.a, "accepted share", || {
        text(
            &pair.a,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &share,
        ) == "active"
    });
    assert_eq!(
        text(
            &pair.a,
            "SELECT local_repo FROM peer_shares WHERE share_id=?1",
            &share
        ),
        pair.ra.to_str().unwrap()
    );
    assert_eq!(
        text(
            &pair.b,
            "SELECT local_repo FROM peer_shares WHERE share_id=?1",
            &share
        ),
        pair.rb.to_str().unwrap()
    );
    let target = pair.target(true, "agent-b");
    received(
        &pair.b,
        &queued(&pair.a, "agent-a", &target, "sync", "explicit mapping"),
    );
}

#[test]
fn worktree_membership_uses_common_repo_and_remote_urls_never_transfer_authority() {
    let (pair, _, target) = shared();
    let worktree = pair.a.root.join("worktree");
    git(
        &pair.a,
        &pair.ra,
        &["worktree", "add", "-b", "topic", worktree.to_str().unwrap()],
    );
    pair.a
        .register_at("worktree-agent", "claude", Some("agent-a"), &worktree);
    let other = repo(&pair.a, "private-q");
    pair.a
        .register_at("q-child", "claude", Some("agent-a"), &other);
    for path in [&pair.ra, &other] {
        git(
            &pair.a,
            path,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/same-repo.git",
            ],
        );
    }
    received(
        &pair.b,
        &queued(
            &pair.a,
            "worktree-agent",
            &target,
            "sync",
            "worktree allowed",
        ),
    );
    refused(
        &pair.a,
        "q-child",
        &target,
        "sync",
        "private q",
        "not_a_member",
    );
    git(
        &pair.a,
        &pair.ra,
        &[
            "remote",
            "set-url",
            "origin",
            "git@example.invalid:changed.git",
        ],
    );
    received(
        &pair.b,
        &queued(&pair.a, "agent-a", &target, "sync", "url changes nothing"),
    );
    refused(
        &pair.a,
        "q-child",
        &target,
        "sync",
        "still private q",
        "not_a_member",
    );
    let clone = pair.a.root.join("separate-clone");
    git(
        &pair.a,
        &pair.ra,
        &[
            "clone",
            "--no-hardlinks",
            pair.ra.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    pair.a.register_at("clone-agent", "claude", None, &clone);
    refused(
        &pair.a,
        "clone-agent",
        &target,
        "sync",
        "same root is only a hint",
        "not_a_member",
    );
    // URL strings above are configuration only: no fetch, clone or remote command runs.
}

#[test]
fn invalid_or_disappeared_repo_fails_closed_even_after_snapshot_cached_it() {
    let (pair, _, target) = shared();
    let outside = pair.a.root.join("work");
    let before = count(&pair.a, "SELECT count(*) FROM peer_shares");
    let invalid = request(
        &pair.a,
        &pair.sa,
        "POST",
        &format!("/api/fed/peers/{}/shares", pair.pa),
        json!({"local_repo":outside,"label":"not-a-repo","inbound":true,"outbound":true}),
    );
    assert_eq!(invalid.status, 400);
    assert_eq!(count(&pair.a, "SELECT count(*) FROM peer_shares"), before);
    pair.sa.snapshot(&pair.a);
    std::fs::rename(&pair.ra, pair.a.root.join("moved-away")).unwrap();
    refused(
        &pair.a,
        "agent-a",
        &target,
        "question",
        "stale cwd",
        "not_a_member",
    );
}

#[test]
fn a_reported_new_cwd_revokes_old_membership_without_inheriting_the_parent_project() {
    let (pair, _, target) = shared();
    let other = repo(&pair.a, "q-new-cwd");
    pair.a
        .register_at("moving-child", "claude", Some("agent-a"), &pair.ra);
    received(
        &pair.b,
        &queued(&pair.a, "moving-child", &target, "sync", "before move"),
    );
    let mut hook = pair.a.pre("claude", "moving-child");
    hook["cwd"] = json!(other);
    pair.a.hook("claude", "PreToolUse", &hook);
    refused(
        &pair.a,
        "moving-child",
        &target,
        "sync",
        "after move",
        "not_a_member",
    );
}

#[test]
fn each_direction_needs_local_outbound_and_remote_inbound_and_reply_needs_reverse_flags() {
    let (pair, share, target) = shared();
    flags(&pair, true, &share, true, false);
    refused(
        &pair.a,
        "agent-a",
        &target,
        "question",
        "outbound disabled",
        "outbound_off",
    );
    flags(&pair, true, &share, true, true);
    flags(&pair, false, &share, false, true);
    eventually(&pair.a, "remote inbound disabled", || {
        count(
            &pair.a,
            "SELECT count(*) FROM peer_shares WHERE remote_inbound=0 AND state='active'",
        ) == 1
    });
    refused(
        &pair.a,
        "agent-a",
        &target,
        "question",
        "inbound disabled",
        "remote_inbound_off",
    );
    flags(&pair, false, &share, true, true);
    eventually(&pair.a, "remote inbound enabled", || {
        count(
            &pair.a,
            "SELECT count(*) FROM peer_shares WHERE remote_inbound=1 AND state='active'",
        ) == 1
    });
    let local = received(
        &pair.b,
        &queued(&pair.a, "agent-a", &target, "question", "question"),
    );
    flags(&pair, false, &share, true, false);
    let before = count(&pair.b, "SELECT count(*) FROM fed_outbox");
    let output = pair
        .b
        .cmd()
        .args([
            "reply",
            &local,
            "--from",
            "agent-b",
            "--body",
            "no reverse authority",
        ])
        .assert()
        .code(1)
        .get_output()
        .clone();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "refused: outbound_off\n"
    );
    assert_eq!(count(&pair.b, "SELECT count(*) FROM fed_outbox"), before);
}

#[test]
fn remote_principals_never_become_local_tree_link_or_grant_edges() {
    let (pair, _, target) = shared();
    pair.a
        .register_at("child-p", "claude", Some("agent-a"), &pair.ra);
    let edges_a = count(&pair.a, "SELECT count(*) FROM edges");
    let edges_b = count(&pair.b, "SELECT count(*) FROM edges");
    received(
        &pair.b,
        &queued(&pair.a, "child-p", &target, "sync", "approved project"),
    );
    for args in [
        vec!["link", "--from", "agent-a", "--to", &target],
        vec![
            "grant", "--from", "agent-a", "--to", &target, "--thread", "thread", "--ttl", "60s",
        ],
    ] {
        pair.a.cmd().args(args).assert().failure();
    }
    pair.a
        .cmd()
        .args([
            "register",
            "--id",
            "forged-child",
            "--harness",
            "claude",
            "--session",
            "forged-child",
            "--cwd",
            pair.ra.to_str().unwrap(),
            "--parent",
            &target,
        ])
        .assert()
        .failure();
    assert_eq!(count(&pair.a, "SELECT count(*) FROM edges"), edges_a);
    assert_eq!(count(&pair.b, "SELECT count(*) FROM edges"), edges_b);
    for bus in [&pair.a, &pair.b] {
        assert_eq!(
            count(bus, "SELECT count(*) FROM agents WHERE id LIKE 'peer:%'"),
            0
        );
        assert_eq!(
            count(
                bus,
                "SELECT count(*) FROM edges WHERE from_id LIKE 'peer:%' OR to_id LIKE 'peer:%'"
            ),
            0
        );
    }
}

#[test]
fn discovery_roster_lists_only_eligible_project_sessions_with_opaque_stable_names() {
    let (pair, _, target) = shared();
    let other = repo(&pair.b, "private-repo-secret");
    pair.b
        .register_at("private-child", "codex", Some("agent-b"), &other);
    pair.b.register_at("closed-agent", "codex", None, &pair.rb);
    pair.b
        .hook("codex", "SessionEnd", &json!({"session_id":"closed-agent"}));
    let session = target.rsplit('/').next().unwrap();
    let output = pair.a.ok(&["peers", "--agent", "agent-a"]);
    let listing = String::from_utf8(output.stdout).unwrap();
    assert!(
        listing.contains("Remote (another person's agents, on their machine; project project):\n")
    );
    let label = format!("codex-{}", &session[..4]);
    assert!(
        listing
            .lines()
            .any(|l| l == format!("  {target}  {label}  idle")
                || l == format!("  {target}  {label}  active")),
        "{listing}"
    );
    for secret in [
        "private-child",
        "closed-agent",
        "private-repo-secret",
        pair.rb.to_str().unwrap(),
    ] {
        assert!(!listing.contains(secret), "discovery leaks {secret}");
    }
    assert_eq!(
        text(
            &pair.b,
            "SELECT session FROM fed_sessions WHERE agent_id=?1",
            "agent-b"
        ),
        session
    );
    let again = pair.a.ok(&["peers", "--agent", "agent-a"]);
    assert!(String::from_utf8_lossy(&again.stdout).contains(&target));
    let intro = hook_context(&pair.a, "claude", "agent-a", &pair.ra);
    assert!(intro.contains("Remote (another person's agents, on their machine; project project):"));
    assert!(intro.contains(&target));
    assert!(intro.contains("untrusted"));
}

#[test]
fn unshare_bumps_revision_removes_discovery_and_deletes_pending_inbound() {
    let (pair, share, target) = shared();
    let message = queued(&pair.a, "agent-a", &target, "sync", "must not inject");
    let local = received(&pair.b, &message);
    let before = count(
        &pair.b,
        "SELECT revision FROM peer_shares WHERE state='active'",
    );
    api(
        &pair.b,
        &pair.sb,
        "DELETE",
        &format!("/api/fed/shares/{share}"),
        json!({}),
    );
    assert_eq!(
        count(
            &pair.b,
            "SELECT revision FROM peer_shares WHERE state='removed'"
        ),
        before + 1
    );
    assert_eq!(
        db(&pair.b)
            .query_row("SELECT count(*) FROM messages WHERE id=?1", [&local], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        count(&pair.b, "SELECT count(*) FROM fed_remote_sessions"),
        0
    );
    assert!(!hook_context(&pair.b, "codex", "agent-b", &pair.rb).contains("must not inject"));
    eventually(&pair.a, "remove propagated", || {
        text(
            &pair.a,
            "SELECT state FROM peer_shares WHERE share_id=?1",
            &share,
        ) == "removed"
    });
    let listing = pair.a.ok(&["peers", "--agent", "agent-a"]);
    assert!(!String::from_utf8_lossy(&listing.stdout).contains(&target));
}

#[test]
fn closed_recipient_is_not_delivered_to_or_retargeted_to_a_new_session() {
    common::share_checks::closed_recipient_is_not_delivered_to_or_retargeted_to_a_new_session();
}

#[test]
fn pending_hook_delivery_rechecks_membership_after_the_repo_disappears() {
    common::share_checks::fresh_delivery_membership();
}

#[test]
fn discovery_crosses_one_page_but_never_exceeds_one_thousand_sessions_per_peer() {
    common::share_checks::discovery_capacity();
}
