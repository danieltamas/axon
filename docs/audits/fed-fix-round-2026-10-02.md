# Federation fix round — 2026-10-02

Merged from four independent reviews of `main...job/fed`: security (`fed-security-2026-10-02.md`),
QA (`fed-qa-2026-10-02.md`), release (`fed-release-2026-10-02.md`) and the Codex cross-vendor review
(CDX-n, quoted in this file). Duplicates are folded; the ids of every source are kept. Each item's
"done when" is the solved criterion a re-review checks.

Rules: src fixes by the implementer; new acceptance tests by Codex (the implementer never authors
the tests it is judged by); the existing frozen suites stay frozen and green.

## Contract changes (owner decisions, binding for both tests and src)

- **C1 — session token (fixes SEC-1).** Browsers send cookies to every port of 127.0.0.1, so the
  cookie alone is not proof of the owner. `POST /api/session` 204 becomes `200 {"token":"<43-char
  base64url>"}`, with the cookie as before. The token is bound to the session (stored hashed next to
  `session_hash`), the page keeps it in `localStorage` (origin-scoped, so another port cannot read
  it), and every `/api/*` request must send `x-axon-session: <token>` matching the cookie's session,
  else `401 {"error":"sign_in"}`. `GET /api/stream` takes it as `?t=<token>` (EventSource cannot set
  headers); the token is never logged. Revoking or expiring a session invalidates its token.
- **C2 — remote lifecycle is visible (fixes the §12 open item; owner requirement "disconnect at
  will").** A received `removed` notice marks the local peer `removed` with `removed_reason =
  'remote_removed'`; a received `paused` notice sets a `remote_paused` flag shown as "Paused by
  <label>" (local state unchanged, sends refused with `peer_paused`); a `resumed` notice is sent on
  resume and clears it. `/api/fed` peers carry `remote_paused: bool`. The Peers card shows both.
- **C3 — over-limit rejections are counted** in `counters.rejected` (memory counter, as §12 says
  "counted, not written"); no audit row per rejection.
- Kept as amended in §12: `recipient_full`; the `messages` FK replaced by triggers with the one-time
  rebuild (verified by security and QA probes: concurrent and kill -9 safe, rows byte-identical).

## P0

| id | finding | done when |
|---|---|---|
| BUG-1 = REL-1 = CDX-1 | bare `axon` panics: `GET /api/health` registered in `src/server.rs:43` and `crates/axon-bus/src/serve.rs:102` | `axon --port N --no-open` stays up; `/api/health` 200; a test that spawns the root binary is green |

## P1

| id | finding | done when |
|---|---|---|
| SEC-1 | session cookie valid on every localhost port | C1 implemented; a cookie without the token gets 401 on every route and on SSE |
| SEC-2 = CDX-5 | revoked session keeps its open `/api/stream` | revoke and expiry close live streams within 1 s |
| SEC-3 = CDX-3 | a remote answer satisfies a local `await_answer` without §8 framing | remote rows never match local answer waits; answers bound to the question's participants |
| CDX-2 = BUG-2 | outbox transmits after outbound off, membership change, pause or remove | grant, state, generation, revision, membership and expiry re-checked immediately before transmit, serialized against revocation |
| CDX-4 | disabling federation from a second process on the same DB leaves the owner process running | the service owner observes persisted enable/relay changes within 2 s |
| CDX-6 | removing share A deletes share B's pending messages after an agent moved | deletion keyed by each message's accepted share id |
| CDX-7 | acknowledged authorization changes can be undone by power loss | `synchronous=FULL` on every federation transaction that precedes a wire ack |
| CDX-8 | capture and share direction switches cannot change (click cancelled, `.set` undone) | form switches change normally; only async switches roll back on server refusal; browser-verified |
| REL-3 | `dist` profile binary is 20.1 MB, budget 15 MB | the shipped profile builds ≤ 15,000,000 bytes; `fed-e2e.sh` measures the shipped profile |
| REL-2 = TEST-1 | no test starts the root binary | covered by BUG-1's test |
| C2 | B shows Connected after A disconnects | C2 implemented and browser-verified on two instances |

## P2

| id | finding | done when |
|---|---|---|
| CDX-9 = BUG-5 | `created_at`/`expires_at` subtraction overflows on peer input | checked arithmetic; overflow rejected as `bad_time` |
| SEC-4 | a peer can hog the write lock; share/discovery/notice frames unrated; unindexed `count(*)` per rejection | per-peer rate limit on every frame kind before any write; the count indexed or replaced by C3 |
| SEC-5 = CDX-14 | offer/remove loop grows shares without bound; no federation retention | tombstones count toward a bounded quota; removed shares not resent; retention sweep for federation tables |
| CDX-10 | reload failure after pause/remove keeps old admission map | reload failure denies and closes; retried on the next tick |
| CDX-11 | concurrent joins can create the same live label | partial unique index on live labels; join fails `label_taken` |
| CDX-12 | expiry, audit, event chain and notice in separate autocommits | one write transaction |
| CDX-13 | restarts leak dialer tasks; handler `Arc` cycle | dialers aborted and joined; weak references |
| CDX-15 | concurrent budget writes lose updates and share a temp file | lock around read-modify-write; unique staging file |
| CDX-16 = BUG-4 | an `unavailable` ack counts as delivered | acks inspected; transient errors retried |
| CDX-17 | `/api/summary` lacks `Cache-Control: no-store` | the cache policy covers the whole API |
| BUG-3 | revision bump silently kills queued messages | the sender is notified, as for expiry |
| QA P2 list | see `fed-qa-2026-10-02.md` | per that report |

## P3

SEC-6..SEC-14, CDX-18 (`..` in refs with `:L`/`@` suffixes), CDX-19 (duplicate `setting()`), QA P3s,
REL P2/P3 docs items (README API pointer, stale SPEC:453, PLAN test name, dead login link on busy
port, FED-MANUAL gaps, MSRV job without `--all-targets`, Windows key ACL untested). Fix where cheap;
otherwise list as accepted in spec §12 with the reason.

## Round 2 — from the Codex re-review (RR-n) and the orchestrator's verification

Verification at fccd1d7 (orchestrator, independent): clippy `-D warnings` clean; `cargo test --workspace
--no-fail-fast` 37 targets green, 1 red (`acceptance_root`: "root axon never opened its dashboard" — the
root binary takes 3.3–5.8 s to listen when run alone, so it misses the 5 s deadline under suite load);
`scripts/fed-e2e.sh` ALL STEPS PASSED, p95 under 1000 ms; `target/dist/axon` 13,680,272 bytes. Browser
evidence for CDX-8 and C2: designer two-instance run on 8aedc94 + ab0b5ff (sign-in without token, form
switches persist, "Paused by", "Removed by them").

| id | finding | done when |
|---|---|---|
| RR-16 (P1) | root `axon` binds seconds after start | the dashboard listens within 1 s of spawn (work before bind moved after it or made lazy); `acceptance_root` green inside the full suite |
| RR-1 (P1) | outbox clearance not serialized with revocation | in the owning process, no frame is transmitted after a pause/remove/unshare/outbound-off commit returns (shared per-peer ordering boundary; no SQLite write txn held across network awaits); cross-process residual window recorded in §12 with its bound |
| RR-2 (P1) | SSE revalidation has no deadline | revalidation is a bounded read-only check (timeout ⇒ close); revoke/expiry ends a live stream within 1 s even with a writer holding the DB; activity bookkeeping separate |
| RR-3/RR-4 (P2) | lifecycle can stick: mutual pause, lost resume, stale notice wins, error ack ends retries | lifecycle carries a per-pairing monotonic sequence; peers reconcile current lifecycle state on every (re)connection, including a control-only connection a remotely paused peer still makes periodically (no message traffic); error acks retried; mutual pause then both resume ⇒ both Connected without re-pairing |
| RR-5 (P2) | reload worker join error keeps stale admission | every reload error variant denies, closes and retries next tick |
| RR-7 (P2) | retention prunes inbox provenance of undelivered messages | pending rows expired/removed in the same transaction before their inbox records; cap never counts unreachable rows |
| RR-8 (P2) | aborted dialers never reaped | completed tasks reaped during operation; shutdown aborts and joins every dialer |
| RR-9 (P2) | budget lock is per process | a shared file lock covers read-modify-rename across processes |
| RR-11 (P2) | ping/unknown frames and address writes bypass the limiter | one admission rate limit before address persistence and dispatch, covering built-ins and unknown types |
| RR-12 (P2) | size check measures a stale binary; 9 latency samples | build failure fails the script; ≥20 latency samples (REL-15) |
| RR-14 (P3) | SEC-12 claimed fixed but key is repaired; SEC-13(b); REL-13 | key file readable by others is refused with a fix-it message; SEC-13(b) accepted in §12 (single-use 60 s nonce, same-user process args); `axon bus guide` states the ref separator per REL-13 |
| RR-13 (P3) | §12 acceptance reasons rest on false facts | SEC-7: relay URL scheme enforced https; SEC-6: neutral local placeholder label instead of the remote-chosen one; SEC-10/BUG-13/SEC-11/REL-4/REL-5/TEST-n: reasons corrected to the true residual risk, or mitigated |
| RR-6, RR-10 | upgrade paths for pre-fix federation databases | accepted in §12: federation has never shipped (job/fed is unmerged), so no database outside development has `fed_inbox` rows or racing duplicate labels |
| RR-15 | execution evidence | recorded above |

## Round 3 — from the Codex round-2 review (R2-n), the last automated round

Evidence at 24e48b7 (orchestrator): clippy clean; full workspace suite exit 0, 45 targets; fed-e2e ALL
STEPS PASSED, p95 825 ms over 25 samples; dist 13,812,752 bytes. b4373cf landed after; re-verify at the end.

| id | finding | done when |
|---|---|---|
| R2-1 (P1) | federation restart swaps the per-peer send gate, so a revocation can race the replacement outbox | one ordering boundary survives service generations (gate owned outside the service handle), or replacement is serialized with revocation until it completes |
| R2-2 (P2) | older connections and in-flight streams survive pause/remove/reload denial | every connection of a peer is tracked; revocation and reload denial close all of them and cancel their handlers; admission is re-checked after a frame is read, before dispatch |
| R2-3 (P2) | control probes are adopted as message send paths | control (probe) connections are marked before adoption and never carry application sends |
| R2-4 (P2) | a stalled peer's backlog starves other peers under the global LIMIT 20 | outbox selection is fair across peers (per-peer cap within the batch) and a failed peer is backed off before it can fill the next batch; §12 BUG-13 states the real bound |
| R2-5 (P2) | the neutral label can collide, consuming the invite without creating a peer | an unused neutral label is allocated in the admission transaction; the invite is consumed only if the peer row was inserted |
| R2-6..R2-9 (P3) | §12 reasons inaccurate: Windows ACL, dashboard nonce in argv, wait/retry bounds, TEST-n | each rewritten to the actual residual (who can exploit it, what bound actually holds); TEST-n lists the remaining gaps (e.g. TEST-3 cap tolerance) with reasons |
