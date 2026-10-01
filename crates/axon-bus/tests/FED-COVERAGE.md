# Federation acceptance contract handoff

Source of truth: `docs/P2P-SPEC.md`, followed by its §11 acceptance mapping.
These are independent process/HTTP/hook tests. No feature source module is imported,
no new dependency is required to compile the integration targets, and no Cargo command
is invoked by a test. Compilation and execution are reserved for the orchestrator.
This handoff does **not** claim complete automated coverage of every mapped row.
Resolve the gaps and assumptions below before treating the suite as a complete freeze.

## Running and isolation

Build the workspace's debug `axon-bus` and `axon` executables before running the six
integration targets. The Settings budget test runs the sibling `axon --scan-only
--no-hooks` executable to exercise the actual config reader; it does not import
`src/config.rs`. The test process locates `axon-bus` through `cargo_bin!("axon-bus")`.

Both entry points were inspected: `src/main.rs` dispatches `axon bus`, while
`crates/axon-bus/src/main.rs` dispatches the shared CLI. Neither currently implements
`open`. The serve-alias login convention chosen here is **`axon-bus open --print`**.
It must print exactly `Dashboard: http://127.0.0.1:<port>/#login=<nonce>` plus a newline.
The helper exchanges the nonce at `POST /api/session`, checks the cookie attributes,
and sends the cookie on subsequent requests. `raw_request` deliberately bypasses it.

Each `Bus::with_limit` gets a separate temp root and cleared environment. HOME, XDG
homes, Claude/Codex/Hermes paths, Windows app-data locations, Git configuration and
all temp paths remain inside that root. All service commands inherit
`AXON_FED_RELAY=disabled` and `AXON_FED_BIND=127.0.0.1:0`. HTTP checks the ready-file
address is IPv4 loopback before connecting. Release-mode server tests fail before
starting a server, because release binaries ignore the debug seams. Git remotes are
metadata only; the clone fixture copies another temporary local repository.

New database assertions open `SQLITE_OPEN_READ_ONLY`. The compact fault alone opens a
write-capable connection to take `BEGIN IMMEDIATE`, changes no rows, and rolls back.
Corruption/permission tests alter only the disposable stopped service's file. They do
not seed authorized peers, messages or audit state with SQL. Existing suites' unrelated
SQL fixtures remain untouched.

Six cases are Unix-only: five suspend/permission cases plus identity permission modes.
Windows runs the kill/restart and corruption equivalents, but does not pretend a kill
is the same fault as a blackhole. A privileged Unix runner that can read a mode-000 DB
prints a specific skip reason for that permission fault.

The invite expiry case has a 630-second budget and deliberately waits on the actual
issuer clock. The nonce expiry case has a 75-second budget. Remaining tests declare
budgets from 12 to 180 seconds. None runs a shell sleep; waits are bounded loops.
Clock-shift tests use `AXON_TEST_NOW_OFFSET_MS` on actual service/CLI/hook processes.

## Row-level coverage and remaining automation

“Partial” describes a missing assertion, not a manual waiver. Only spec §11's three
manual items are labeled manual in `acceptance_fed_lifecycle.rs`.

| Rows | Implemented evidence | Remaining evidence |
|---|---|---|
| A01 | Public assets disclose no token/agent data; protected reads/writes/SSE reject cookies/legacy headers; nonce use/expiry, cookie attributes, hash-shaped persistence, restart and revocation | Browser fragment removal and rendered sign-in panel; independently recompute SHA-256 once digest input representation is fixed |
| A02 | Ready address is loopback and Host/Origin guards reject rebinding | QUIC listener cannot serve dashboard protocols; socket bind inspection beyond the announced address |
| A03 | HTTP-only pairing, both confirmations, no shares/discovery, denied send | None beyond assumptions below |
| A04 | Replacement/cancel, four-versus-five attempts, persisted attempts, same-node retry, simultaneous distinct joiners, nonce/invite/confirmation deadlines | Exact millisecond equality for the ten-minute boundary; startup-offset tests bracket it with margins |
| A05 | Tampered node pin refuses without consuming the invite or incrementing its authentication attempts; incorrect code removes authority | Capture proof that the secret never left the process; authenticated unpinned raw transport |
| A06 | Unsupported invitation version cannot create a peer | ALPN/version downgrade, unknown frames, pre-auth data and 0-RTT require a raw transport probe |
| A07 | Different local paths, no-remote repo, worktree common dir, unrelated repo, local clone with same root commit, URL change | SSH-versus-HTTPS equivalence is represented by local URL metadata changes, not external Git traffic |
| A08 | Wrong-project child, updated cwd, missing repo after cached snapshot, fresh hook delivery membership | Malicious receiver envelope with forged membership/share |
| A09 | Each direction flag refused independently, positive send, reply denied without reverse outbound | Reverse remote-inbound reply variant |
| A10 | Shared child send; link/grant/remote-parent refusal; no remote agents or edges | A third peer transit attempt |
| A11 | Forbidden CLI kinds and recipient tables unchanged; permitted informational kinds | Malicious wire sender IDs, extra privileged fields and receiver-only kind rejection |
| A12 | Exact quoted hook frame, fake terminator/owner text/shell/markup remain body, inert unavailable ref | Browser text rendering and network-fetch instrumentation |
| A13 | Real question/reply, wrong local addressee, preserved thread/reply_to, frozen receipt provenance after rename | Forged answer/ack from another authenticated peer/share/session |
| A14 | Accepted message and inbox identity survive receiver kill; no repeated hook injection | Forced lost ack, concurrent replay, same-ID content conflict and duplicate wire response |
| A15 | Sender killed after enqueue preserves message; receiver killed after acceptance preserves inbox | Deterministic pre-commit and post-commit/pre-ack process kills |
| A16 | Idle receipt changes only transport counters; hook preparation creates no answer | Kill a hook precisely after delivery commit/before stdout |
| A17 | Stable original TTL, restart at hour 23, expiry after hour 24 with sender sync, expired pending inbox, 119s/121s future clock | Raw excessive lifetime, exact 120000/120001 ms receipt-time boundary |
| A18 | Pause survives both restarts; pending hook text suppressed; explicit resume restores delivery; exact paused send refusal | Queued outbound suspension/expiry during pause and repeated hostile dials |
| A19 | Unshare deletes pending inbound and discovery; offline remove cancels queued output and atomically exposes revoked state | Deterministically stop at validation/commit and selection/transmission windows; stale revision replay |
| A20 | Removal survives restart; old target refused; same node re-pairs with a different generation and zero live shares/queues/discovery | Raw old-generation replay after re-pairing |
| A21 | Roster allowlist, project exclusion, closed-session exclusion, opaque stable identifiers | Inspect complete serialized wire DTO; observed-only session fixture |
| A22 | Unshare invalidates listing; closed recipient not retargeted; replacement has a new session ID | Raw discovery request against revoked cached grants; exact 60s stale-cache boundary |
| A23 | 400/401 Unicode scalars, refs 8/9 and 256/257 bytes, threads 128/129 bytes, C0 except newline/tab | 8192/8193 frame prefix, malformed/deep JSON, UUID fields, NUL wire byte |
| A24 | Unsafe relative/absolute/URL/shell/ref forms refused; unavailable valid ref stays metadata | Instrument arbitrary outbound fetches/checkout operations at receiver |
| A25 | 1000/1001 outbox rows; 100/101 pending inbox rows; 20/21 hook count and 16 KiB byte cap; discovery crosses 100 and caps at 1000 | Receiver burst/rate boundaries, encoded-frame pages, queue byte/global caps, rejection-audit cap |
| A26 | Blackholed peer does not slow ordinary local hook beyond 300 ms; local delivery works; unreadable/corrupt storage cannot claim queued/direct send | Disk-full and exact write-lock failure branches inside remote accept |
| A27 | CLI with killed server queues without transmission; restart delivers it once; second server logs the lock holder | OS socket enumeration proving ordinary CLI/hooks create no listeners |
| A28 | SSE updates within 5s plus scheduling margin, blackhole offline/stale by 30s plus one SSE interval, healthy HTTP, recovery and unchanged counters | Exact EWMA alpha and each exponential-backoff step |
| A29 | Unix 0700/0600, symlink destination preservation, missing/corrupt key alongside peers disables without regeneration | Genuine failed chmod/ACL fault; other-account access is manual per §11 |
| A30 | Manual per §11 | Power loss and rollback/restore review |
| A31 | Audit/event count and decision correspondence, chain verification, no body/cookie/invite prefix, stable fingerprint after rename | Audit-write failure rollback, independent payload-hash computation, raw key/invite-secret/log scan |
| A32/A34 | All specified Settings and lifecycle API actions exercised with owner sessions | Browser interaction is not established by API checks |
| A33 | Reject or wrong code leaves neither side with a non-removed peer; invite consumed | Uses spec tombstone semantics, not physical deletion |
| S1 | Settings persistence, capture effect in hooks, budgets in real root CLI, hook state in doctor, federation doctor lines | No specified doctor output for capture/usage settings |
| S2 | Old/new captured transcript rows either side of a retention cut; independent narrative and usage windows; null keeps usage | Exact cutoff equality requires a general clock seam |
| S3 | All four harnesses, repeated install, uninstall, preserved foreign entries/comments/config | None |
| S4 | Real SQLite write reservation produces 409 busy within 1s plus scheduling margin, unchanged agents and integrity | Reservation is held by the test connection, not a hook stopped at its commit boundary |
| S5 | Comment-preserving config update; real `axon --scan-only --no-hooks` returns matching budget fields | None |

A raw iroh probe cannot be linked by the current package manifest: it has no iroh or
QUIC dependency, and only test directories are editable. No imaginary debug HTTP
endpoint, feature source include, ignored placeholder, or unconditional failing test
has been added to disguise that gap. A separately built test-only probe needs a
specified build invocation and key/frame serialization contract. Precise transaction
crash windows and forced chmod failure need additional observable fault controls or
an external OS fault harness. These are outstanding **automated** requirements.

## Contract ambiguities and chosen interpretations

1. §1: “`axon open` does the same for a server that is already running.” Neither current
   entry point has this command. Assumption: the shared serve alias supplies
   `axon-bus open --print`; the helper does not compile against an absent API. The
   second-server test assumes the newly ready dashboard becomes the `open` target;
   the spec does not define selecting between dashboards sharing a data directory.
2. §4: “both sides ... show the fingerprints and the pair code.” §10's example peer
   object omits `pair_code`, and join/confirm success response bodies are unspecified.
   Assumption: each pending peer in `GET /api/fed` includes a string `pair_code`. Its six
   groups and symmetry are asserted. Success codes not fixed by the spec use 2xx.
3. §3: “first 24 decimal digits derived from `sha256(min(node_a, node_b) ‖ max(node_a,
   node_b))`.” Decimal conversion, node ID byte representation, and leading zeros are
   not defined. No independently computed pair-code value is frozen. Fingerprints are
   checked as four groups of four hex digits, with stable peer provenance.
4. §4: “`secret` is 32 random bytes.” Its JSON representation is unspecified.
   Assumption: a 43-character unpadded base64url string, matching the nonce convention.
5. §4: “the peer is removed”; A33 says “no peer row.” Spec §§5/9 retain removed rows.
   Assumption: no live authority, with a removed tombstone allowed. §1 likewise
   overrides A01's old-browser rejection after restart: sessions survive restarts.
6. §3: “State becomes `offline`”; §5's stored peer state CHECK has no `offline` value.
   Interpretation: health is live state; stored `active` remains the authority floor,
   as §10 explicitly says. Paused/removed are asserted in both storage and health.
7. §7 fixes refusal reasons, but does not map invalid refs, thread grammar or C0 bytes
   to a particular one, or order overlapping CLI refusals. These malformed-input
   cases require exit 1, an exact `refused: <listed reason>` line, and no inserted row.
   The zero-share synthetic target currently expects `unknown_session`.
8. §8: “at most 20 messages and 16 KiB of remote text per hook call.” Assumption:
   16 KiB counts UTF-8 bytes of the complete remote frames; local introduction text is
   outside that budget. The reference-free test frames are delimited before counting.
9. §2: “`~/.config/axon/config.toml` ... `src/config.rs` reads the same file.”
   Interpretation: honor XDG_CONFIG_HOME, as the inspected root CLI already does.
10. §0: “Shifts the clock federation uses.” It does not authorize shifting session or
    transcript clocks. Nonce/invite deadlines use real waits where needed; retention
    uses captured timestamps and a one-minute margin. Timed HTTP/SSE assertions allow
    scheduling/observation margin, not a different configured timeout.
11. §8's 1000-row and 8192-byte frame ceilings imply at most 8,192,000 wire bytes per
    peer, below 8 MiB (8,388,608). The independent byte cap is unreachable through
    valid frames if `fed_outbox.bytes` means serialized frame bytes. Specify whether
    additional accounted storage makes this boundary independently reachable.
12. §8 only correlates `answer` in pipeline step 7, whereas A13 asks to correlate
    forged `ack` too. The current tests enforce the specified legitimate reply path;
    the malicious-ack policy must be resolved for the raw probe.

## Existing-suite migration

Only authentication changed in `acceptance_audit_http.rs` and `acceptance_m5.rs`.
The latter's token-specific 403/rotation assertions became §1's 401 sign_in and
restart-surviving cookie assertions, with comments identifying the superseding spec.
The SSE request now carries the cookie. Other existing assertions were retained.

`acceptance_audit_ingest.rs`, `acceptance_m3.rs` and `acceptance_m4.rs` contain no
HTTP/token authentication in this checkout, so their assertions/files were not edited.
There is no root `tests/common/http.rs`; the actual helper is
`crates/axon-bus/tests/common/http.rs`, which was migrated in place.

## Verification status

The author ran Rustfmt's parser/formatter and whitespace/scope/line-count checks only.
No Cargo build, Cargo test, manual rustc build, or feature process was run. Type-check
success and the expected red runtime results remain unverified until the orchestrator
compiles and executes the suite. All new test bodies use existing dependencies and
process interfaces, so absent feature Rust modules cannot cause missing-import errors.
