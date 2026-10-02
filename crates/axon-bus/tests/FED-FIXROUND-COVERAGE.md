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

## Round 2

Authority: the **Round 2** done-when column in the work order above, with
`docs/P2P-SPEC.md` for the HTTP, wire, retention and capacity contracts. In particular,
RR-3/RR-4 supersede section 12's earlier acceptance of a permanently lost resume.
Assertions were derived from those contracts and existing test helpers, without reading
the feature implementation being changed in parallel. Existing test cases are unchanged;
the two existing shared helper files have additions only.

The table names exact Rust test functions. Files are under `crates/axon-bus/tests/`.

| RR id | New acceptance coverage or explicit gap |
|---|---|
| RR-16 | `acceptance_root_fixround2.rs`: `root_dashboard_listens_within_one_second_plus_two_hundred_ms_slack`. Starts the prebuilt root binary in a fresh home, with no preceding init/warm-up process; TCP must accept within 1,200 ms measured before spawn. Authenticated HTTP health and process survival follow outside the latency budget. |
| RR-1 | `acceptance_fed_fixround2_revocation.rs`: `pause_return_is_a_wire_barrier_for_a_busy_outbox`, `remove_return_is_a_wire_barrier_for_a_busy_outbox`, `unshare_return_is_a_wire_barrier_for_a_busy_outbox`, `outbound_off_return_is_a_wire_barrier_for_a_busy_outbox`. Twelve real CLI sends, a positive wire receipt, held message acknowledgements and an unsent-backlog precondition precede the owner API call. A barrier releases the acks as that call begins, racing transmission with revocation. Observe for 12 seconds after its return, including reconnections; every later message frame fails. |
| RR-2 | `acceptance_session_fixround2.rs`: `revoked_stream_closes_within_one_second_while_another_connection_holds_the_writer`. Revoke through HTTP, immediately hold `BEGIN IMMEDIATE` on a separate real SQLite connection, prove another writer is blocked, and require the SSE socket to close by one second from revocation return. Repeat three times and verify the surviving session still works. Expiry under contention is not covered because this addition targets the requested revocation case; the federation clock seam does not specify a dashboard-session clock. |
| RR-3 | `acceptance_fed_fixround2_lifecycle.rs`: `both_paused_peers_resume_to_connected_without_repairing` and `lost_resume_notice_reconciles_remote_paused_after_the_other_owner_resumes`. Both sides must report `connected` with `remote_paused=false` within 30 seconds; original peer ids, generations and the single pairing remain. Stale sequence injection is not covered because no lifecycle sequence field is specified in the current wire contract. |
| RR-4 | The same two lifecycle tests. The lost-notice case resumes B while A remains paused, then resumes A while B is stopped and restarts B to discard buffered notices. Recovery must follow reconnection without another resume or pairing. Explicit error-ack retry injection is not covered because the existing probe does not impersonate both ends of a real paired lifecycle exchange. |
| RR-5 | Not covered because no existing seam injects a reload worker join failure at the service boundary; corruption/read faults do not establish that specific failure branch. |
| RR-6 | Not covered because the work order accepts the pre-fix development-database upgrade path rather than defining a new runtime requirement. |
| RR-7 | `acceptance_fed_fixround2_wire.rs`: `retention_of_undelivered_inbox_provenance_does_not_steal_the_hundredth_slot`. Receive a real pending message, restart the receiver with `AXON_TEST_NOW_OFFSET_MS` advanced 91 days, observe the old inbox row disappear, accept 100 fresh wire messages to the same recipient, and reject the 101st with `recipient_full`. The expired local row must be gone. No hook drains the recipient and no message rows are seeded. |
| RR-8 | Not covered because task reaping/join completion has no process-level task inventory or fault seam; a surviving process would not prove dialers were reaped. |
| RR-9 | `acceptance_settings_fixround2.rs`: `concurrent_processes_updating_different_budgets_preserve_both_writes`. Two real servers with separate DBs share one config directory through a Unix symlink. Forty synchronized pairs of HTTP writes change different budget keys; each pair must persist both fresh values, retain the third budget and owner comments, and survive restart. Separate DBs prevent SQLite serialization from masking a process-local config lock. |
| RR-10 | Not covered because the work order explicitly accepts the pre-fix development-only duplicate-label upgrade path. |
| RR-11 | `acceptance_fed_fixround2_wire.rs`: `ping_flood_is_rate_limited_and_rejections_are_counted` and `unknown_frame_flood_is_rate_limited_and_rejections_are_counted`. A normal frame establishes the expected response; 100 concurrent requests on the authenticated connection must complete within three seconds and include `rate_limited` errors, reflected in `counters.rejected`. Receiver remains healthy. Address-persistence ordering is not covered because the public boundary exposes no per-frame write trace. |
| RR-12 | Not covered because build-failure propagation and release-script sample counts are tooling checks outside these process/HTTP acceptance additions; this work order prohibits running builds. |
| RR-13 | `acceptance_fed_fixround2_policy.rs`: `join_refuses_http_relay_invite_without_consuming_the_valid_invite`. Change only a valid invitation's relay URL to loopback `http://`; expect `invalid_invite`, no peer rows, and successful joining with the original invite. Neutral join labels are **not covered because** section 12 still accepts SEC-6 without defining a local placeholder or its naming rule; skipped as requested, rather than inventing one. Other acceptance-reason/documentation corrections are not covered because they require review of the corrected rationale, not an HTTP assertion. |
| RR-14 | `acceptance_fed_fixround2_policy.rs`: `enabling_federation_refuses_0644_identity_without_repairing_it` and `starting_federation_refuses_0644_identity_without_repairing_it`. Create a genuine key through Settings, make it 0644, require refusal with a diagnostic naming `identity.key` and the 0600 repair, and preserve both mode and key bytes. Diagnostic may be in the response, health or stderr (section 12 permits fixed public `unavailable` errors). The accepted nonce-argv risk and guide separator wording are not covered because they are outside the requested key/relay cases. |
| RR-15 | Not covered because this is orchestrator execution evidence, not a new acceptance behavior; no runtime pass/fail claim is made here. |

Round 2 harness and verification limits:

- All subprocesses retain the existing disposable HOME/XDG isolation and
  `AXON_FED_RELAY=disabled` / `AXON_FED_BIND=127.0.0.1:0` seams. No external network,
  production home, schema fabrication or feature-source import is used.
- `common/wire_probe.rs` adds a bounded concurrent burst and an observer entry point;
  accepting the federation ALPN lets the probe observe reconnects too.
  `common/wire_observer.rs` records completed message frames with monotonic timestamps,
  answers control traffic and releases held message acks as the revocation call begins.
  The hello response is captured from the real peer because its response DTO is not
  specified. Test-created observer tasks are aborted and joined on drop.
- RR-1 observes the owning process at a real receiver, not only inbox persistence.
  Holding a received request's ack first establishes a busy outbox; releasing it alongside
  the API call avoids serializing away the race. The cutoff uses receiver timestamps, as
  requested; it cannot identify when buffered network bytes were originally transmitted.
  It does not exhaust every scheduler interleaving, prove the internal lock placement,
  or measure the cross-process residual revocation window.
  The coverage obligation for documenting that window remains with the implementer.
- RR-2 acquires the writer immediately **after** the revoke commit; SQLite cannot commit
  revocation while a different writer retains its reservation. This covers the contended
  revalidation interval, with a scheduling window between the HTTP return and lock
  acquisition. The writer is never released just to let the stream assertion pass.
- RR-7 spaces fresh arrivals by 510 ms to avoid conflating the recipient's 2/s token
  bucket with its 100-message pending cap. A probe control responder handles heartbeats
  during this longer case. Atomic cleanup at every possible intermediate interleaving
  is not separately proven by the final capacity/row assertions.
- The lost-notice, shared-config and mode-bit cases are Unix-only (signals, symlink and
  Unix permissions). Windows ACLs and a Windows shared-config fixture remain uncovered.
  RR-9 is a repeated real concurrency regression, not exhaustive proof of a file lock.
- **Not runtime verified:** no cargo command, build or test was run, as instructed.
  Only formatting and static diff/scope/line-count checks were performed. The
  orchestrator must compile and execute these additions, including the root test alone
  when measuring the one-process startup contract. Red results are expected for unfixed
  requirements; they must not be relaxed to fit current source behavior.
