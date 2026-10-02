# Federation fix-round acceptance contract

Binding work order: [fed-fix-round-2026-10-02.md](../../../docs/audits/fed-fix-round-2026-10-02.md),
including C1–C3. Existing assertions changed only for C1 authentication: login is 200
with a 43-character base64url token; authenticated HTTP uses cookie plus
`x-axon-session`; SSE uses cookie plus `?t=`. Restart/revocation cases keep both
credentials. Raw requests intentionally bypass automatic authentication.

All new cases use disposable HOME/XDG directories. No Cargo command is invoked by
the tests. The root smoke test requires the already-built `axon` sibling of
`cargo_bin!("axon-bus")` and uses `--port <free-port> --no-open --no-hooks`.

The existing iroh dependency now permits `common/wire_probe.rs`: a real paired
process is killed, its disposable key is loaded, and a replacement endpoint binds
IPv4 loopback with relays and discovery disabled. It sends length-prefixed frames
to the actual receiver; no feature source module is imported and no federation
rows are seeded. CDX-9 is implemented, not skipped.

The migration fixture is the exact bus schema from main commit
`988ee52f444bb3e77b645c5ccd6585c32a0a8ca8`. Only this migration test creates legacy
schema/rows and tests SQL constraint writes. Offline queue tests take and roll back
a write reservation before SIGSTOP, without changing rows. All federation state comes from real
CLI, HTTP, hooks and wire traffic.

## Test mapping

Names are the Rust test functions. Expected behavior below is inferred from the
starting source, not an observed run. On that tree C1's 204 response first blocks
all tests using authenticated setup; the deeper failures describe what each
regression targets once C1 is implemented.

| Test | Work-order id | Expected failure on starting source |
|---|---|---|
| `cookie_without_session_token_cannot_get_post_or_open_sse` | C1, SEC-1 | Cookie alone authorizes GET, POST and SSE |
| `token_from_another_session_cannot_authorize_cookie_or_stream` | C1, SEC-1 | Token is not bound to the cookie |
| `revoked_session_token_is_denied_and_its_live_stream_closes_within_one_second` | C1, SEC-2, CDX-5 | Existing stream survives revocation |
| `root_dashboard_stays_alive_and_serves_health_and_summary` | BUG-1, REL-1, CDX-1, REL-2, TEST-1 | Duplicate health route panics before readiness |
| `remote_answer_with_local_question_thread_and_refs_cannot_satisfy_blocking_ask` | SEC-3, CDX-3 | Remote body satisfies local ask instead of timeout default |
| `local_blocking_ask_accepts_only_the_questions_addressee` | SEC-3, CDX-3 | Wrong local sender satisfies ask |
| `outbound_off_prevents_offline_queue_delivery_after_reconnect` | CDX-2, BUG-2 | Queued traffic can beat share update after reconnect |
| `pause_prevents_offline_queue_delivery_after_receiver_restart` | CDX-2 | Stale in-flight batch regression; may already pass absent that race |
| `remove_prevents_offline_queue_delivery_after_receiver_restart` | CDX-2 | Stale in-flight batch regression; may already pass absent that race |
| `membership_change_prevents_offline_queue_delivery_after_reconnect` | CDX-2 | Outbox does not recheck sender membership |
| `unsharing_old_project_preserves_pending_messages_accepted_on_new_project` | CDX-6 | Unshare deletes the moved agent's second-share messages |
| `extreme_envelope_timestamps_are_bad_time_without_panicking_receiver` | CDX-9, BUG-5 | Overflow produces unavailable/panic instead of bad_time |
| `removal_notice_marks_remote_peer_removed_with_reason` | C2 | Remote peer remains active; reason missing |
| `remote_pause_is_visible_blocks_sends_and_resume_clears_it` | C2 | remote_paused absent and notices ineffective |
| `over_audit_limit_rejections_increment_health_counter_without_more_audit_rows` | C3 | Counter stops at the ten-row audit quota |
| `concurrent_joins_with_one_label_have_exactly_one_winner` | CDX-11 | Both different inviters can acquire the same live label |
| `old_main_message_fk_migrates_without_loss_and_enforces_local_sender_integrity` | Retained §12 migration; QA TEST-5 | Expected to pass: retained migration safeguard |

## Verification limits

No `cargo build` or `cargo test` was run, as instructed. Formatting, file-size and
diff checks do not prove compilation or runtime results. The implementer/orchestrator
must compile these tests against the current workspace and observe the failures.

The four offline-queue cases and simultaneous-join case are Unix-only because they
use SIGSTOP/SIGCONT. Offline tests kill the receiver to avoid delivering bytes that
were sent before revocation, then restart it and observe its inbox for 12 seconds.
Outbound and membership cases also require reconnection, preventing a disconnected
receiver from producing a false pass. The tests permit local cancellation/rejection
policy choices and do not prove the absence of all network transmissions. Pause and
remove intentionally prevent a usable connection. They do not resume/re-pair and
therefore do not revoke the condition being tested. Scheduling-sensitive cases need
runtime review; a passing run alone is not proof of every revocation interleaving.

SEC-3 permits the security report's rejection or namespacing of a colliding thread,
but always requires the local ask to return its default. The raw overflow case also
requires a subsequent valid frame to succeed on the same connection. Browser UI and
power-loss behavior are outside these additions.
