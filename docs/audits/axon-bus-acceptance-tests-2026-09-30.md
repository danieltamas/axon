# Independent acceptance tests: TEST-1 through TEST-12

Run date: 2026-09-30. Branch: `job/axon-bus/m0`.
Implementation HEAD: `77a5662f4df2de0895443e3aca613e73c694e737`.

**57 tests executed: 54 passed, 3 failed, 0 ignored.** All three failures are TEST-9 Codex config preservation defects. No failing acceptance assertion was weakened, and no implementation or existing test file was modified.

Sources: [audit solved criteria, Operator decisions and Fix round](axon-bus-2026-09-30.md), [BUS-PLAN sections 3, 6 and 7](../BUS-PLAN.md). Tests use the existing `Bus`/`Server` harness, captured hook/transcript fixtures, the real binary via `assert_cmd`, and binary-created SQLite databases.

## Files and commands

Every file is new, English-only, and below 500 lines. Commands were run individually from the repository root.

| New file | Lines | Passed | Failed | Command exit | Environment |
|---|---:|---:|---:|---:|---|
| [acceptance_audit_gate.rs](../../crates/axon-bus/tests/acceptance_audit_gate.rs) | 335 | 23 | 0 | 0 | sandbox |
| [acceptance_audit_ingest.rs](../../crates/axon-bus/tests/acceptance_audit_ingest.rs) | 236 | 8 | 0 | 0 | sandbox |
| [acceptance_audit_install.rs](../../crates/axon-bus/tests/acceptance_audit_install.rs) | 324 | 11 | 3 | 101 | sandbox |
| [acceptance_audit_narrative.rs](../../crates/axon-bus/tests/acceptance_audit_narrative.rs) | 357 | 7 | 0 | 0 | unsandboxed localhost |
| [acceptance_audit_http.rs](../../crates/axon-bus/tests/acceptance_audit_http.rs) | 165 | 5 | 0 | 0 | unsandboxed localhost |

```sh
cargo test -p axon-bus --test acceptance_audit_gate
cargo test -p axon-bus --test acceptance_audit_ingest
cargo test -p axon-bus --test acceptance_audit_install
cargo test -p axon-bus --test acceptance_audit_narrative
cargo test -p axon-bus --test acceptance_audit_http
```

The initial sandbox runs failed before server readiness for six narrative tests and two HTTP tests. Their common failure text was:

```text
serve exited before readiness
```

A separate socket bind to `127.0.0.1:0` confirmed the environment restriction:

```text
Loopback binding blocked: PermissionError(1, 'Operation not permitted')
```

Both suites then ran outside the sandbox against isolated ephemeral localhost servers: narrative 7/7 and HTTP 5/5 passed. No test was skipped. The per-test results below use these completed runs.

## Per-finding test names and results

| Finding | Test name | Result | File |
|---|---|---|---|
| TEST-1 | `test_1_claude_budget_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_claude_healthy_control` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_claude_stop_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_codex_budget_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_codex_healthy_control` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_codex_stop_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_hermes_budget_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_hermes_healthy_control` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_hermes_stop_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_opencode_budget_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_opencode_healthy_control` | PASS | `acceptance_audit_gate.rs` |
| TEST-1 | `test_1_opencode_stop_query_error` | PASS | `acceptance_audit_gate.rs` |
| TEST-2 | `test_2_peer_stop_survives_budget_raise` | PASS | `acceptance_audit_gate.rs` |
| TEST-2 | `test_2_raising_child_budget_preserves_root_stop_until_root_is_raised` | PASS | `acceptance_audit_gate.rs` |
| TEST-2 | `test_2_stop_hook_acks_peer_but_budget_stop_still_denies` | PASS | `acceptance_audit_gate.rs` |
| TEST-3 | `test_3_budget_crossing_resolves_registered_root_session_alias` | PASS | `acceptance_audit_gate.rs` |
| TEST-3 | `test_3_session_alias_doorbell_denies_without_hub` | PASS | `acceptance_audit_gate.rs` |
| TEST-3 | `test_3_stop_hook_acks_alias_and_removes_both_doorbells` | PASS | `acceptance_audit_gate.rs` |
| TEST-3 | `test_3_stop_resolves_registered_root_session_alias` | PASS | `acceptance_audit_gate.rs` |
| TEST-4 | `test_4_usd_only_budget_denies_unpriced_captured_model` | PASS | `acceptance_audit_ingest.rs` |
| TEST-4 | `test_4_usd_only_priced_under_ceiling_allows` | PASS | `acceptance_audit_ingest.rs` |
| TEST-4 | `test_4_usd_only_zero_usage_allows` | PASS | `acceptance_audit_ingest.rs` |
| TEST-5 | `test_5_budget_denial_stands_when_first_crossing_cannot_commit` | PASS | `acceptance_audit_gate.rs` |
| TEST-5 | `test_5_failed_stop_commit_keeps_unacked_stop_and_doorbell` | PASS | `acceptance_audit_gate.rs` |
| TEST-5 | `test_5_pending_budget_stop_keeps_child_doorbell` | PASS | `acceptance_audit_gate.rs` |
| TEST-5 | `test_5_stop_hook_removes_child_doorbell_after_commit` | PASS | `acceptance_audit_gate.rs` |
| TEST-6 | `test_6_one_agent_keeps_independent_cursors_for_two_paths` | PASS | `acceptance_audit_ingest.rs` |
| TEST-6 | `test_6_partial_trailing_line_is_ingested_once_after_completion` | PASS | `acceptance_audit_ingest.rs` |
| TEST-6 | `test_6_reingest_and_append_count_each_turn_once` | PASS | `acceptance_audit_ingest.rs` |
| TEST-6 | `test_6_shrunk_file_is_reread_from_zero` | PASS | `acceptance_audit_ingest.rs` |
| TEST-6 | `test_6_two_agents_sharing_a_path_keep_independent_cursors` | PASS | `acceptance_audit_ingest.rs` |
| TEST-7 | `test_7_bad_transcript_path_keeps_stop_hook_successful` | PASS | `acceptance_audit_narrative.rs` |
| TEST-7 | `test_7_stop_hook_captures_last_assistant_line_in_snapshot` | PASS | `acceptance_audit_narrative.rs` |
| TEST-7 | `test_7_subagent_stop_does_not_attribute_main_thread_rows_to_child` | PASS | `acceptance_audit_narrative.rs` |
| TEST-8 | `test_8_replay_expiry_deletes_old_rows_and_keeps_inside_row` | PASS | `acceptance_audit_narrative.rs` |
| TEST-8 | `test_8_restart_without_content_hides_prior_text_but_keeps_structure` | PASS | `acceptance_audit_narrative.rs` |
| TEST-8 | `test_8_snapshot_omits_rows_older_than_seven_days_and_keeps_inside_row` | PASS | `acceptance_audit_narrative.rs` |
| TEST-9 | `test_9_claude_byte_exact_roundtrip` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_claude_preserves_edits` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_claude_stale_backup` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_codex_byte_exact_roundtrip` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_codex_inline_edit_roundtrip_control` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_codex_preserves_edits` | **FAIL** | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_codex_stale_backup` | **FAIL** | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_codex_uninstall_after_reserialized_edit_removes_hooks` | **FAIL** | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_hermes_byte_exact_roundtrip` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_hermes_preserves_edits` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_hermes_stale_backup` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_opencode_byte_exact_roundtrip` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_opencode_preserves_edits` | PASS | `acceptance_audit_install.rs` |
| TEST-9 | `test_9_opencode_stale_backup` | PASS | `acceptance_audit_install.rs` |
| TEST-10 | `test_10_audit_stdout_head_equals_last_event_hash` | PASS | `acceptance_audit_http.rs` |
| TEST-10 | `test_10_different_event_chains_have_different_heads` | PASS | `acceptance_audit_http.rs` |
| TEST-11 | `test_11_localhost_origin_with_valid_token_returns_201` | PASS | `acceptance_audit_http.rs` |
| TEST-11 | `test_11_localhost_origin_wrong_port_returns_403_without_inserting` | PASS | `acceptance_audit_http.rs` |
| TEST-11 | `test_11_self_send_fails_without_panic_or_new_row` | PASS | `acceptance_audit_http.rs` |
| TEST-12 | `test_12_replay_redacts_secrets_in_snapshot_and_every_database_text_column` | PASS | `acceptance_audit_narrative.rs` |

## Failure evidence

The edits change only the parsed `model` value. Serializing the resulting TOML represents hook arrays as `[[hooks.Event]]` tables; the helper reparses the output and asserts semantic equality before running the binary. The separate inline-edit control passes. The failed expectations remain that valid owner edits survive reinstall and that uninstall removes every bus hook.

**`test_9_codex_preserves_edits`**: reinstall exits 1. Verbatim error excerpt:

```text
config.toml `hooks.SessionStart` is not an array
```

**`test_9_codex_stale_backup`**: reinstall exits 1. Verbatim error excerpt:

```text
config.toml `hooks.PreToolUse` is not an array
```

**`test_9_codex_uninstall_after_reserialized_edit_removes_hooks`**: uninstall exits 0, but all seven bus hook commands remain. The acceptance assertion fails. Verbatim failure excerpt:

```text
bus entry survived uninstall: model = "owner-edited"

[[hooks.PostToolUse]]
command = "/Users/danime/Sites/axon/target/debug/axon-bus hook codex PostToolUse"
```

The combined reinstall/uninstall tests stop at the failing reinstall. The separate uninstall test exercises and proves the second failure independently.

## Contract interpretations and coverage limits

- TEST-3 uses `session-s` for the audit's symbolic session identifier S. Root ID remains `orch`. The tests do not assume uppercase IDs use plain filenames, because the Fix round documents SEC-9 filename hardening.
- TEST-5 induces an actual COMMIT failure: a rollback-journal reader permits a writer to begin and update, then blocks COMMIT with `SQLITE_BUSY`. A SQLite control proves this distinction. The binary tests assert rolled-back row state, retained doorbells, and a denial on the first budget crossing with no preexisting stop.
- TEST-8 says "Replay with --content"; BUS-PLAN section 7 assigns `--content` to `serve`. Following the spec and established M5 contract, tests run replay under a live `serve --content` lease, then restart without capture. Expiry is exercised through the binary's replay operation; no private expiry function is called.
- TEST-9 does not define "stale backup." The test leaves the backup from an earlier install, replaces the live config with a newer hook-free owner config, and requires that current edits survive the next install and uninstall. This applies the owner-edit preservation contract to an explicit recovery scenario.
- TEST-10 follows the documented `prev_hash` schema: the next append must record the reported head as its previous event hash. This checks the last event hash without duplicating the implementation's hashing algorithm.
- TEST-11 UI draft preservation is declared manual-only in `acceptance_audit_http.rs`; no DOM test was written or run.
- TEST-12 checks `[redacted]` in assistant, reasoning, progress and tool rows, plus scans every text-valued cell in every SQLite table for the original credential. "Every DB text column" means no plaintext credential anywhere; unrelated text cells are not required to contain a redaction marker.
- These additions cover TEST-1 through TEST-12. They do not certify every separate SEC/BUG finding or the full workspace. Green tests ran against already implemented behavior; no source mutations were used to manufacture red runs.

## Additional validation

```sh
cargo clippy -p axon-bus --test acceptance_audit_gate --test acceptance_audit_ingest --test acceptance_audit_install --test acceptance_audit_narrative --test acceptance_audit_http -- -D warnings
rustfmt --check --edition 2021 --config skip_children=true crates/axon-bus/tests/acceptance_audit_gate.rs crates/axon-bus/tests/acceptance_audit_ingest.rs crates/axon-bus/tests/acceptance_audit_narrative.rs crates/axon-bus/tests/acceptance_audit_install.rs crates/axon-bus/tests/acceptance_audit_http.rs
```

Clippy and formatting checks passed. `git diff --exit-code` confirmed no changes to `crates/axon-bus/src`, `acceptance_m1.rs` through `acceptance_m5.rs`, or `tests/common`. The new files and this report are uncommitted.

NOT VERIFIED: complete TEST-9 acceptance; three Codex config tests remain red. The other 54 tests passed.
