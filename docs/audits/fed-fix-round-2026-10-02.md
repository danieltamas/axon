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
