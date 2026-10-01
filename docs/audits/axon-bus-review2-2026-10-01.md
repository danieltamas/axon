Independent review 2 — 2026-10-01

Reviewed `61f7916..cca4fe8` on `job/axon-bus/m0`: 12 commits. The last commit changed documentation only. Reviewed the `crates`/`src` diff and only the requested UI JavaScript: `sw.js`, `pwa.js`, `offline.js`, `send.js`. Findings 2 and 8 remain out of scope.

Added 36 acceptance tests: **26 PASS, 8 FAIL, 2 Windows-only tests not run on macOS**. All existing source and test content is preserved byte-for-byte; additions are nine `#[cfg(test)] mod acceptance_review2` modules and `tests/acceptance_review2.rs`. No production code was edited; no commit was made. Every termination call supplies an impossible negative start time and exercises refusal only. Doctor tests use temporary directories, a shared static mutex, and restoration of the four layout environment variables.

| Finding | Verdict | Evidence and remaining criterion |
|---|---|---|
| 1 | PARTIAL | Requests require `started_ms` and reject mismatches ([end.rs:35](../../crates/axon-bus/src/end.rs)); however, [memory.rs:148](../../crates/axon-bus/src/memory.rs) checks time then calls `kill_with`. Locked sysinfo 0.33.1 uses `kill(pid, signal)` on macOS/Linux, leaving a PID-reuse race. Start times are second-resolution values multiplied by 1000 (line 155). Solved when signalling is bound to the verified process generation, or unverifiable targets are refused. |
| 4 | PARTIAL | Requested commented headers, indentless sequences and flow-map refusal pass. [hermes_hooks.rs:98](../../crates/axon-bus/src/hermes_hooks.rs) still derives indentation from comments; a column-zero comment puts added events outside `hooks`. See N1. |
| 5 | PARTIAL | [install.rs:98](../../crates/axon-bus/src/install.rs) and line 137 preserve `bus` and recognize Windows executable paths. But [cli_guard.rs:32](../../crates/axon-bus/src/cli_guard.rs) and line 38 erase backslashes before `invocation` sees them. A double-quoted `C:\Program Files\Axon\axon.exe --port 7777` command is not refused. Shared parser replay confirms this; native Windows execution/quoting remains unverified. |
| 6 | FIXED | [ingest/mod.rs:70](../../crates/axon-core/src/ingest/mod.rs), [ccflare.rs:54](../../crates/axon-core/src/ingest/ccflare.rs), and [opencode.rs:21](../../crates/axon-core/src/ingest/opencode.rs) propagate read/SQLite errors as `None`. [main.rs:218](../../src/main.rs) skips successful-scan stamps on `None`. Readable foreign-schema ccflare databases return `Some(empty)`. |
| 7 | PARTIAL | CLI tests prove the stored fingerprint includes `PARSER_VERSION` and `BUNDLED` ([main.rs:198](../../src/main.rs)). An ordinary meta change invalidates its stamp. However, lines 262–275 retain size/max-mtime collisions: metadata-preserving rewrites, sidecar mtimes hidden by a newer transcript, and compensating DB/WAL sizes all fail. Solved when each relevant file's content identity changes the cache key even if metadata collides. |
| 9 | PARTIAL | Claude missing PreToolUse and unregistered OpenCode shims are rejected ([doctor.rs:23](../../crates/axon-bus/src/doctor.rs), line 54). Hermes still searches raw substrings at line 64: commenting out every hook leaves it “wired.” Solved when every required hook is checked as an active event entry. |
| 10 | PARTIAL | All three numeric token examples mask, and the requested usage counts remain readable. [transcript.rs:373](../../crates/axon-bus/src/transcript.rs) still uses suffix/substring exemptions instead of explicit count fields and now exposes previously masked credentials. See N2. |

New defects, ranked:

1. **N1 — P1: a comment moves Hermes enforcement hooks outside their mapping.** At `hermes_hooks.rs:98–105`, `hooks:\n# Owner documentation\n  owner_event:\n    - command: 'beacon'` gives `key_indent = 0`. Installing adds `pre_tool_call:` as a top-level key; uninstall does not remove it, and Hermes does not receive that hook under `hooks`. A source replay at `61f7916` preserved nesting and round-tripped; HEAD fails both. **Solved when:** indentation comes from YAML content/event keys, comments cannot change nesting, and both comment fixtures round-trip exactly.
2. **N2 — P2: redaction now discloses credentials that previously masked.** At `transcript.rs:373`, `password_tokens=12345678` and `secret_token_count=12345678` are treated as usage counts. Baseline replays mask both; HEAD returns both unchanged, affecting the shared storage/display redactor. **Solved when:** only explicit supported usage-count keys bypass masking and secret-named keys remain masked.
3. **N3 — P2: the assignment fix creates a direct-command guard bypass.** At `cli_guard.rs:95`, any executable word containing `=` is ignored. `'/tmp/a=b/axon' --port 7777` is a real executable path, but can enable capture without refusal. Baseline refuses it; HEAD allows it. **Solved when:** only actual shell assignment words are skipped; the two requested assignment tests and this executable-path test all pass.
4. **N4 — P2: service-worker activation deletes other applications' caches.** At `ui/sw.js:14`, activation deletes every cache except its own current name. Reusing a localhost origin/port that has another app's offline cache erases that cache. Executing the actual activation handler with cache keys `unrelated-app-offline-data`, `axon-offline-old`, and the current name deleted the first two. **Solved when:** cleanup deletes only obsolete Axon-owned cache names and preserves unrelated caches.

Each independent acceptance test is listed below. A failing assertion is retained as a source finding, not weakened to match current behavior. The initial End fixture used `store::open` on a missing temporary database; setup was corrected to `store::init` without changing its refusal assertions.

| Test | Result |
|---|---|
| [r1_request_requires_process_generation](/Users/danime/Sites/axon/crates/axon-bus/src/end.rs:53) | PASS |
| [r1_end_refuses_impossible_generation](/Users/danime/Sites/axon/crates/axon-bus/src/end.rs:64) | PASS |
| [r1_terminate_refuses_generation_different_from_sample](/Users/danime/Sites/axon/crates/axon-bus/src/memory.rs:182) | PASS |
| [r1_terminate_refuses_generation_different_from_live_process](/Users/danime/Sites/axon/crates/axon-bus/src/memory.rs:190) | PASS |
| [r4_commented_hooks_merge_once](/Users/danime/Sites/axon/crates/axon-bus/src/hermes_hooks.rs:189) | PASS |
| [r4_unwire_retains_indentless_event_keys](/Users/danime/Sites/axon/crates/axon-bus/src/hermes_hooks.rs:206) | PASS |
| [r4_indentless_wire_unwire_restores_original](/Users/danime/Sites/axon/crates/axon-bus/src/hermes_hooks.rs:215) | PASS |
| [r4_top_level_comment_inside_hooks_preserves_mapping_and_roundtrip](/Users/danime/Sites/axon/crates/axon-bus/src/hermes_hooks.rs:220) | FAIL |
| [r4_plain_hooks_comment_preserves_existing_owner_event](/Users/danime/Sites/axon/crates/axon-bus/src/hermes_hooks.rs:231) | FAIL |
| [r4_install_merges_commented_hooks_without_duplicate_mapping](/Users/danime/Sites/axon/crates/axon-bus/src/install.rs:467) | PASS |
| [r4_install_refuses_flow_mapping](/Users/danime/Sites/axon/crates/axon-bus/src/install.rs:481) | PASS |
| [r5_windows_binary_names_keep_dispatch_and_are_recognized](/Users/danime/Sites/axon/crates/axon-bus/src/install.rs:488) | PASS |
| [r5_recognizes_existing_quoted_windows_hooks](/Users/danime/Sites/axon/crates/axon-bus/src/install.rs:514) | PASS |
| [r5_windows_command_uses_cmd_compatible_quotes](/Users/danime/Sites/axon/crates/axon-bus/src/install.rs:528) | NOT RUN — `cfg(windows)` |
| [guard_assignment_is_not_an_invocation](/Users/danime/Sites/axon/crates/axon-bus/src/cli_guard.rs:238) | PASS |
| [guard_assignment_prefix_does_not_hide_dashboard](/Users/danime/Sites/axon/crates/axon-bus/src/cli_guard.rs:249) | PASS |
| [r5_invocation_recognizes_windows_binary_names](/Users/danime/Sites/axon/crates/axon-bus/src/cli_guard.rs:258) | PASS |
| [r5_windows_backslash_command_is_guarded](/Users/danime/Sites/axon/crates/axon-bus/src/cli_guard.rs:275) | NOT RUN — `cfg(windows)` |
| [guard_equals_in_executable_path_is_still_an_invocation](/Users/danime/Sites/axon/crates/axon-bus/src/cli_guard.rs:288) | FAIL |
| [r6_missing_sources_are_failures](/Users/danime/Sites/axon/crates/axon-core/src/ingest/mod.rs:227) | PASS |
| [r6_unreadable_sqlite_sources_are_failures](/Users/danime/Sites/axon/crates/axon-core/src/ingest/mod.rs:252) | PASS |
| [r6_readable_foreign_ccflare_schema_is_successful_empty_parse](/Users/danime/Sites/axon/crates/axon-core/src/ingest/mod.rs:270) | PASS |
| [r7_subagent_meta_change_invalidates_stamp](/Users/danime/Sites/axon/src/main.rs:436) | PASS |
| [r7_meta_change_older_than_transcript_invalidates_stamp](/Users/danime/Sites/axon/src/main.rs:451) | FAIL |
| [r7_metadata_preserving_rewrite_invalidates_stamp](/Users/danime/Sites/axon/src/main.rs:470) | FAIL |
| [r7_wal_and_db_size_changes_do_not_cancel](/Users/danime/Sites/axon/src/main.rs:487) | FAIL |
| [r7_scan_cache_key_includes_parser_version](/Users/danime/Sites/axon/tests/acceptance_review2.rs:60) | PASS |
| [r7_scan_cache_key_includes_bundled_prices](/Users/danime/Sites/axon/tests/acceptance_review2.rs:71) | PASS |
| [r9_claude_missing_pretooluse_is_not_wired](/Users/danime/Sites/axon/crates/axon-bus/src/doctor.rs:157) | PASS |
| [r9_opencode_unregistered_shim_is_not_wired](/Users/danime/Sites/axon/crates/axon-bus/src/doctor.rs:175) | PASS |
| [r9_commented_hermes_hooks_are_not_wired](/Users/danime/Sites/axon/crates/axon-bus/src/doctor.rs:190) | FAIL |
| [r10_numeric_access_token_is_masked](/Users/danime/Sites/axon/crates/axon-bus/src/transcript.rs:458) | PASS |
| [r10_numeric_id_token_is_masked](/Users/danime/Sites/axon/crates/axon-bus/src/transcript.rs:466) | PASS |
| [r10_numeric_refresh_token_is_masked](/Users/danime/Sites/axon/crates/axon-bus/src/transcript.rs:471) | PASS |
| [r10_explicit_usage_counts_remain_readable](/Users/danime/Sites/axon/crates/axon-bus/src/transcript.rs:476) | PASS |
| [r10_secret_named_fields_are_not_usage_counts](/Users/danime/Sites/axon/crates/axon-bus/src/transcript.rs:490) | FAIL |

Verification evidence:

- `cargo test --workspace` → exit **101**, stopped at three scan-stamp assertion failures.
- `cargo test --workspace --no-fail-fast` → exit **101**, reached all targets. HTTP tests failed before server readiness. A separate `socket.bind(('127.0.0.1', 0))` probe returned `PermissionError(1, 'Operation not permitted')`: local binding is sandbox-blocked.
- Fallback `cargo test --workspace --lib --no-fail-fast` → exit **101**: **75 passed, 5 failed**. All five remaining library failures are independent acceptance assertions described above.
- `cargo test -p axon --bin axon --test acceptance_review2 --no-fail-fast` → exit **101**: main **1 passed / 3 failed**; isolated scan CLI **2 passed / 0 failed**.
- `node --check` passed for all four reviewed JavaScript files. Service-worker activation was replayed in a Node VM; this establishes the deletion logic, not end-to-end browser installation/offline behavior.
- `git diff --check` passed. A byte-prefix comparison against HEAD proved all pre-existing source/test content unchanged.
- Temporary source replays under `/tmp/axon-review2-replay` compared `61f7916` and HEAD for redaction, guard parsing and Hermes editing without changing either checkout revision. The full acceptance suite was not run against the baseline: generation and parse-result APIs differ there.

Selected raw outputs:

```text
# Final library suite
axon:      6 passed; 0 failed
axon-bus: 41 passed; 5 failed
axon-core:28 passed; 0 failed

# Metadata-preserving transcript rewrite
left: (13, 1700000000000)
right: (13, 1700000000000)

# New numeric-secret regression
left: "password_tokens=12345678"
right: "password_tokens=[redacted]"

# Actual service-worker activation handler replay
deleted: ["unrelated-app-offline-data", "axon-offline-old"]
```

Logs: `/tmp/axon-review2-workspace.log`, `/tmp/axon-review2-all-targets.log`, `/tmp/axon-review2-libs-final.log`, `/tmp/axon-review2-scan-final.log`.

NOT VERIFIED: Windows runtime/quoting, browser install/offline flows, and live HTTP checks blocked by local-bind sandbox restrictions. Process-signalling success was deliberately never exercised.
