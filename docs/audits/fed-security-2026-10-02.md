# Security audit: `job/fed` against `main` (federation, owner session, Settings)

- Date: 2026-10-02. Auditor: Severin (security persona), read-only. Scope: `git diff main...job/fed` (114 files) plus the enclosing functions of every touched module.
- Contract: `docs/P2P-SPEC.md` (with §12 Amendments), `docs/P2P-PLAN.md` §6, `docs/reviews/P2P-COUNCIL-gpt.md`.
- **Decorrelation notice.** The auditor runs on Claude. The implementation commits look Claude-authored; the frozen acceptance suite is marked Codex-authored. Treat this audit as **non-authoritative**: a cross-vendor pass (Codex or the council) is still required before sign-off. The change touches auth and a new network trust boundary, so the council should review it before it ships.
- Test independence: the only edits to the frozen suites after `85cca1e` are in `3bfb133`, and they are lint-only (an unused import, `0` → `0o000`, `assert!` → `compile_error!`). None of them changes behaviour.

## Stance

**0 P0 · 1 P1 · 4 P2 · 9 P3.**

The most important finding is SEC-1. The owner cookie is the dashboard's only credential, and it is not bound to the dashboard's port. The browser sends it to every HTTP server on `127.0.0.1`. Once anything holds the cookie, the Origin check does not stop it, because any non-browser client can set that header. The serve.rs doc comment claims "another local account sees only the sign-in screen". That claim is false.

Evidence was gathered in two ways:
- **Probe.** An isolated `axon-bus serve` instance (scratch `HOME`, `XDG_*`), driven with curl.
- **Migration test.** The branch's `axon-bus init` was run over a pre-federation database built from main's schema.

The rest comes from reading the code.

---

## P1

### SEC-1 (P1): The owner cookie is port-agnostic, so any 127.0.0.1 listener receives it, and a holder bypasses the Origin "CSRF" check
- **Location:** `crates/axon-bus/src/session.rs:152` (`cookie_header`), `crates/axon-bus/src/serve.rs:212-260` (`guard`), and the claim at `serve.rs:5-7`.
- **What is wrong:** The cookie is `axon_session=…; HttpOnly; SameSite=Strict; Path=/`, with no port binding. Cookies are not isolated by port (RFC 6265 §8.5). The browser therefore sends `axon_session` to `http://127.0.0.1:<any port>/`. `SameSite=Strict` does not help, because every 127.0.0.1 port is the same site.
- **Scenario A (another OS account, or a sandboxed agent that cannot read the DB but can bind a port):**
  1. The attacker binds `127.0.0.1:5173` and serves a "preview".
  2. The owner opens it. Agents print preview links like this all day.
  3. The request carries `axon_session` (`HttpOnly` only stops JavaScript, not the receiving server).
  4. The attacker replays the cookie with curl. A forged Origin is accepted, as the probe showed: `cookie POST compact, forged own Origin (non-browser): 200`.
  5. That gives full owner authority: install or uninstall hooks in the owner's real `~/.claude` and `~/.codex` (`/api/settings/hooks/*`), create invites and join attacker peers, share repos, send as the operator (`/api/msg`), read every snapshot and message body, and VACUUM.
- **Scenario B (port squatting):** While `axon` is stopped, the other account binds `127.0.0.1:7777`. The owner's browser, or the installed PWA, sends the 30-day cookie to it.
- **Why it matters:** This breaks the "OS-account boundary" required by council F1 (`P2P-COUNCIL-gpt.md:9`). The Origin check only constrains browsers. It is not a credential.
- **Solved criteria:**
  1. The session credential the browser sends to the dashboard is not sent to a different port on the same host. One way: serve on a unique hostname such as `http://axon-<random>.localhost:<port>`, accept it in `loopback_host`, and scope the cookie to that host. Another way: require a second, origin-bound proof on every `/api` request that a cookie alone cannot supply.
  2. A test shows that a request presenting only the leaked cookie, with a forged `Origin`, gets 401/403 on every `/api/*` write and on SSE.
  3. The serve.rs module comment matches the real guarantee.
- **Fix direction:** A per-install unique `*.localhost` host isolates the cookie from other loopback ports. Pair it with a per-session token held in page memory and sent as a header, so a stolen cookie is not enough on its own.

## P2

### SEC-2 (P2): Revoking a session ("Sign out other browsers") does not end its open `/api/stream`
- **Location:** `crates/axon-bus/src/serve.rs:229-233`. The session is checked once, when the request arrives. `stream()` (`serve.rs:352-370`) and `fed::api::events` never check it again.
- **Scenario:** An attacker who obtained a cookie (see SEC-1) opens `/api/stream`. The owner notices and clicks "Sign out other browsers". New requests from the attacker get 401, but the open stream keeps sending snapshots (including message and narrative bodies when capture is on) and the `fed` view (peers, `local_repo` paths, health) until the TCP connection drops. Session expiry does not end it either.
- **Evidence (probe):** After `revoke_others` from session B, a request from session A got `GET /api/fed: 401`, yet A's open SSE went from 2 to 4 events in the next 9 s.
- **Solved criteria:**
  1. Within at most N seconds (for example 5) of `revoke_others`, or of the session's expiry, every SSE response for that session ends.
  2. A test opens a stream with session A, revokes from B, and asserts the stream closes and sends no further `snapshot` or `fed` events.
- **Fix direction:** Re-run `session::valid` on each emit or every few seconds inside the stream, and end the stream when it fails. Or keep a revocation `watch` channel that the streams select on.

### SEC-3 (P2): A remote `thread` lands in the local thread namespace, so a peer can answer a local blocking `ask`, unframed
- **Location:**
  - `crates/axon-bus/src/fed/receive.rs:322-344`: `store_message` writes `msg.thread` verbatim into `messages.thread`.
  - `crates/axon-bus/src/msg.rs:283-301`: `await_answer` matches `kind='answer' AND thread=? AND refs_json=json_array(question)`, with no `from_id` or `to_id` filter.
  - `msg.rs:119`: a local thread defaults to the question's own id, `m-<16 hex>`.
  - `envelope.rs:24-30,40-49`: both `thread_ok` and `refs_ok` accept `m-<hex>`.
- **Scenario:**
  1. A local agent Y runs `axon bus ask --to X …`. Its question id and thread are both `m-abc…`.
  2. The id leaks to a peer. For example, a local agent cites it with `--ref m-abc…` or `--thread m-abc…` in a remote send. That is a natural thing to do, because local message ids are shown to agents in every frame.
  3. A malicious peer replies to any remote question we sent it (`reply_to` valid, so step 7 passes). The reply carries `thread: "m-abc…"` and `refs: ["m-abc…"]`.
  4. The receiver stores the row under the local thread. Y's `ask` returns `{"body": <remote text>, "timed_out": false}` as if X had answered. There is no remote framing and no "another person's agent" marker. This is exactly the forged local answer or approval that §8 framing is meant to prevent.
  5. Smaller effects: remote rows join local thread timelines in the dashboard, and they make the recipient "in thread" for `route::grant` (`route.rs:91-97`).
- **Status:** The code path is confirmed. Exploitation depends on the local message id leaking.
- **Solved criteria:**
  1. A remote message can never be returned by `await_answer` for a local question. The query requires `from_id` to equal the question's addressee, or excludes `peer:%` rows.
  2. A remote message whose thread equals an existing local thread with local participants is either stored under a peer-namespaced thread (the wire value is kept for replies; the frozen test `question_reply_maps_exact_participants…` asserts it) or rejected.
  3. A regression test sends a remote `answer` with `thread` and `refs` set to a pending local `ask`, and asserts that the `ask` times out with its default.
- **Fix direction:** Namespace remote threads on receipt, for example `fed:<peer_id>:<thread>` locally, mapped back on reply. Add the sender filter to `await_answer`.

### SEC-4 (P2, PLAUSIBLE): A paired peer can starve the hub's write lock and break local hooks
- **Location:**
  - `fed/receive.rs:155-158`: every `msg` frame opens SQLite and takes `BEGIN IMMEDIATE` with `synchronous=FULL` before any check.
  - `receive.rs:263-275`: the rate limit runs only after checks 1–7, so rejected frames are never rate-limited.
  - `fed/shares/inbound.rs:91`, `fed/discovery.rs:146`, `fed/pairing/wire.rs:86`: the share, discovery and confirmed/notice frames also take a write transaction each, with no rate limit at all.
  - `fed/audit.rs:31-38`: each rejection runs `count(*)` over `fed_audit`, which has no index and no retention.
  - `store.rs:228`: hooks give up after 150 ms (`HOOK_BUSY_TIMEOUT`).
  - `fed/transport.rs:76-99`: one spawned task per QUIC stream, bounded only by the stream credit.
- **Scenario:** An active peer, malicious or buggy, fills its concurrent streams with `discovery` frames or invalid `msg` frames (wrong share id). Each one holds the write lock for an fsync, a filesystem resolution, or an unindexed scan that grows with the audit table. Local hooks miss their 150 ms window, and agents on this machine lose bus delivery and stop/budget enforcement while the flood lasts. Separately, within spec limits, a peer can push 16 KiB of remote text into an agent's context on every hook call (2/s per recipient), which drives up token cost.
- **Status:** Not exercised under load.
- **Solved criteria:**
  1. A per-peer token bucket is enforced before any SQLite connection or transaction, for every frame type (not only `msg`).
  2. `fed_audit` has an index on `(peer_fingerprint, decision, ts)`.
  3. A load test with 100 concurrent invalid frames per second from one peer keeps local hook p99 latency under the hook timeout.
- **Fix direction:** Move the peer rate check in front of `spawn_blocking` in `FedProtocol::accept`, and cap in-flight requests per connection (a semaphore). Add the index.

### SEC-5 (P2): Peer-driven offer/remove cycles grow `peer_shares` without bound and amplify every reconnect; no federation table has retention
- **Location:**
  - `fed/shares/inbound.rs:157-165`: the cap counts only `state<>'removed'`.
  - `inbound.rs:225-238`: a peer may remove its own offered share.
  - `fed/shares/sync.rs:132-142`: `unsettled` resends `removed` shares on every (re)connect.
  - Repo-wide there is no `DELETE FROM` on `peer_shares`, `fed_inbox`, `fed_outbox`, `fed_audit`, `fed_sessions` or `peer_invites`, and `events` is append-only by trigger.
- **Scenario:**
  1. An active peer loops: `share_offer` ×50, then `share_remove` ×50, with fresh ids each time.
  2. Every cycle leaves 50 permanent `removed` rows, with no rate limit (SEC-4).
  3. On every reconnect, our side spawns one `push` task per removed row, ever, so 10⁵ rows means 10⁵ tasks and frames per reconnect.
  4. Accepted messages also add permanent `fed_inbox`, `fed_audit` and `events` rows at 10/s per peer.
- **Solved criteria:**
  1. The rows a peer can make us store are bounded in total, removed rows included. Either removed rows are purged after the peer has acknowledged them, or offers past a lifetime cap are refused.
  2. Reconnect resends at most a bounded number of share frames.
  3. Expired `fed_inbox` rows beyond the replay window, terminal `fed_outbox` rows and old `fed_audit` rows have a retention sweep.
  4. A test runs 1,000 offer/remove cycles and asserts that the `peer_shares` row count stays at or below the cap.
- **Fix direction:** Treat `removed` as resendable only until the peer answers once, then delete the row. Add a nightly retention sweep beside `transcript::expire_if_due`.

## P3

### SEC-6 (P3): The joiner chooses the inviter's label for it, and `label_taken` lets an invite holder list the inviter's peer labels
- **Location:** `fed/pairing/admit.rs:110-118` stores `request.label` (the joiner's name *for the inviter*, "Name this peer", `ui/settings-pair.js:142-157`) as the inviter's label *for the joiner*. `admit.rs:110-117` answers `label_taken` after a valid secret without consuming the invite or counting an attempt.
- **Scenario:**
  1. Someone who stole a blob joins as `bob`. The inviter's screen then shows "Pending: bob", and agent contexts later show `peer:bob/<session>`. The label is attacker-chosen, so it supports social engineering during confirmation.
  2. With the secret, the joiner can probe `label_taken` as often as it likes for 10 minutes, and so learn the labels of the inviter's other peers.
  3. Functionally, both sides end up showing the same name for different nodes.
- **Solved criteria:**
  1. The inviter stores a label that its own owner chose (or a neutral `axon-<fingerprint4>` placeholder until the owner renames it), never one the remote supplied.
  2. `label_taken` is not reachable by a remote. A clash is resolved locally.
- **Fix direction:** Ignore `request.label` on the inviter side and assign a local placeholder.

### SEC-7 (P3, PLAUSIBLE): An invite's `relay_url` accepts any scheme and host, so pasting a hostile invite makes the owner's machine reach out to that host (blind SSRF)
- **Location:** `fed/invite.rs:208-212` accepts any `RelayUrl`, including `http://` and internal hosts. Settings, by contrast, insists on `https://` (`settings/input.rs:127-131`).
- **Scenario:** The owner pastes `axon1:` from "a collaborator". iroh dials the relay URL from the owner's machine to an internal or metadata host, and the 16 `direct_addrs` get QUIC probes.
- **Status:** iroh's dial behaviour against such a relay was not exercised.
- **Solved criteria:**
  1. `invite::parse` refuses any `relay_url` that is not `https://`, and refuses any whose host is private, loopback or link-local (debug seam excepted).
  2. A unit test covers `http://` and `https://169.254.169.254`.
- **Fix direction:** Apply `relay_is_valid` plus a private-range deny in `invite::parse`.

### SEC-8 (P3): The pair-code check on the server is circular, so all of the protection rests on the human comparing digits
- **Location:** `fed/api.rs:313-315` sends `pair_code` to the page. `ui/settings-peers.js:48-53` posts that same value back. The check at `fed/pairing.rs:166-176` therefore cannot fail from the UI, and `pair_code_mismatch` is unreachable.
- **Scenario:** An owner who clicks "Confirm, it matches" without comparing pairs with whoever used the blob first. That is a design property, but the spec's "mutual pair code" wording suggests the server enforces something.
- **Solved criteria:** Either the page asks the owner to type the other screen's code (and only that is posted), or the spec and UI state plainly that the server only records the owner's attestation.
- **Fix direction:** Type-to-confirm with, for example, the last 8 digits.

### SEC-9 (P3): The `peer:` partition uses case-insensitive `LIKE`
- **Location:**
  - `store.rs:73-78`: the FK-replacement triggers use `NOT LIKE 'peer:%'`.
  - `msg.rs:438`: the local delivery filter.
  - `fed/delivery.rs:96`.
  - The `/api/msg` guard at `serve.rs:388` and `remote.rs:62` use case-sensitive `starts_with("peer:")`.
- **Evidence:** In the migrated test database, inserting `from_id='PEER:ghost'` succeeded with no agents row, while `'ghost'` was refused with `FOREIGN KEY constraint failed`.
- **Scenario:** Any local sender id that starts with `PEER:` or `Peer:` skips the sender FK, and its messages drop out of local delivery. It is local-only, but the partition is a trust boundary.
- **Solved criteria:** The triggers and filters use `substr(from_id,1,5)='peer:'` (or `GLOB 'peer:*'`), and a test shows that `'PEER:x'` is refused by the FK trigger.

### SEC-10 (P3): Supply chain
- **Finding:** `cargo deny check advisories` on the branch reports:
  - **RUSTSEC-2026-0285, rustls 0.23.40** (TLS 1.3 handshake messages accepted across key changes). It was already in main through reqwest, but federation puts it on an internet-facing QUIC path through iroh and noq. The transcript is still authenticated, so the impact is low.
  - **RUSTSEC-2026-0257, webbrowser 1.2.1.** The URL passed here is a fixed `http://127.0.0.1…#login=<base64url>` with no spaces, so it is not exploitable as used.
  - **RUSTSEC-2026-0190, anyhow** (`downcast_mut` unsound). The branch does not call it.
  - **RUSTSEC-2024-0436, paste** is unmaintained.
- **Other notes:**
  - The branch adds 168 crates (323 → 491).
  - With `relay = "default"`, `presets::N0` publishes the node's presence and home relay to n0's DNS and relays. That is a third-party metadata disclosure the Settings copy should state.
- **Solved criteria:** `cargo update -p rustls` to 0.23.45 or later and `-p webbrowser` to 1.2.2 or later; `cargo deny check advisories` is clean or carries documented ignores; the Settings federation section names the default relay operator.

### SEC-11 (P3, PLAUSIBLE): `axon/pair/1` is open to any dialer at all times
- **Location:** `fed/pairing/wire.rs:220` registers the pair handler whether or not an invite is open. `fed/transport.rs:107-165` has 8 slots, each with a 15 s timeout. `admit.rs:68-70` takes `BEGIN IMMEDIATE` with `synchronous=FULL` for any well-formed request.
- **Scenario:** Anyone who has ever seen one of the owner's invites knows the node id. They can hold all 8 slots continuously, which blocks pairing, and force write transactions.
- **Solved criteria:** With no open invite, a pair connection is refused before the handshake completes. A request whose `invite_id` is unknown is answered without a write transaction.

### SEC-12 (P3): A group- or world-readable identity key is silently chmod-ed and used
- **Location:** `fed/identity.rs:44-58`. `read_key` reads the key, then calls `restrict_to_owner`, which fixes the mode. Its own doc comment says it "Refuses a file others can read".
- **Scenario:** The key was exposed, for example restored from a backup at 0644. Federation keeps running on a key that may have been copied, and nobody is told.
- **Solved criteria:** A key file with mode bits beyond 0600, or owned by another uid, makes `load_or_create` fail with `KEY_LOST`. Peers show the error. A test covers the 0644 case.

### SEC-13 (P3, PLAUSIBLE): Two framing and secret edges
- **(a) Line separators in bodies.** `envelope::body_ok` (`envelope.rs:18-23`) rejects only the Cc category. U+2028 and U+2029 pass. `delivery::frame_of` (`delivery.rs:139-141`) splits only on `\n`, so such a body renders as one "│ " line. Any renderer or tokenizer that treats U+2028 as a newline would see unprefixed lines.
  - **Solved criteria:** `body_ok` refuses U+2028, U+2029 and bidi controls (U+202A–U+202E, U+2066–U+2069), or the framing escapes them. A test covers each.
- **(b) The login nonce in a process argument.** It is printed on stdout and passed to the browser. On Linux, `webbrowser` runs `xdg-open <url>`, so other local users can read the URL in `/proc/*/cmdline` within its 60 s life.
  - **Solved criteria:** The nonce never appears in a child process's argv. For example, the browser opens a fixed URL and the page fetches the nonce through a channel only the owner can read.

### SEC-14 (P3): The share lookup in `remote::reply` does not filter by peer
- **Location:** `fed/remote.rs:79-87`. The `fed_audit` subquery matches on `message_id` and `generation` only.
- **Scenario:** A second peer that reuses a first peer's `message_id`, at the same small generation number, can make our reply to the first peer resolve to its own share. The reply is then refused (`unknown_session`). This fails closed, so it can block replies but cannot misroute them. The delivery-side query (`delivery.rs:89-92`) does filter by peer.
- **Solved criteria:** The subquery joins `peer_shares` on `share_id`, with `peer_id = i.peer_id`, or filters `a.peer_fingerprint`.

---

## Checked and clean

**Dashboard auth (probed on an isolated instance):**
- With no cookie, `/api/fed`, `/api/settings`, `/api/stream` and `/api/snapshot` all return 401.
- POSTs with no Origin or a foreign Origin get 403.
- A rebinding `Host` gets 403.
- A login nonce works once: 204, then 401 on replay. It expires after 60 s (`session.rs:68-75`).
- Nonces and sessions are stored only as SHA-256 hashes and looked up by primary key, so there is no secret comparison to time.
- The server generates the cookie, so session fixation is not possible.
- `Cache-Control: no-store` is set on `/api`.
- No GET route has side effects.
- `require_owner` covers `/api/summary` and the usage `/api/health`.
- The UI uses `textContent` everywhere; there are no `innerHTML` sinks in `ui/`.
- `#login=` is removed with `replaceState`.

**Pairing:**
- The secret is 256 bits and the invite id 128 bits.
- The constant-time hash compare is at `invite.rs:139-147`.
- Attempts are counted and committed inside `BEGIN IMMEDIATE`. The 5th kills the invite.
- The first joiner consumes the invite. A retry by the same node only succeeds with the secret.
- The joiner pins the inviter's node id through iroh before sending the secret.
- The invite secret is never in `/api/fed`, logs, `fed_audit` or SQL (only its hash is stored).
- A pending peer has no authority: msg, share and discovery frames all require `state='active'`.
- The generation is agreed, capped at 2^40, and checked on every frame.

**Authorization across the wire:**
- The `axon/fed/1` gate refuses non-peers after the handshake (`transport.rs:43-53`) and re-checks per stream.
- Every handler re-reads the `peers` row by node id and generation.
- Receiver-side, every frame is checked for:
  - an active share owned by that peer, with an equal revision and local `inbound`;
  - recipient membership resolved on the spot;
  - the kind allowlist and a `reply_to` provenance check for `answer`;
  - dedup by content hash.
- Unshare, pause and remove cancel queued rows and delete undelivered rows. Delivery re-checks the peer, the generation, the share and membership.
- No local path, repo name or agent id crosses the wire: discovery sends an opaque 60-bit session id, `<harness>-<4>` and availability; `root_commit` is only a hint.
- A remote sender is always `peer:<local label>/<12-char session>`, so it cannot impersonate a local sender or another peer.
- Every DB error, timeout or unavailable branch in the handlers answers an error or `unavailable` and stores nothing.

**Prompt framing:**
- Every body line is prefixed with `│ `.
- Bodies refuse \r, ESC and other Cc characters.
- Refs are regex-bounded with no spaces, and only listed.
- The header fields (label, session, kind, thread) are all charset-restricted.
- Remote rows are excluded from the local delivery block.

**Denial of service (bounds that hold):**
- The 8 KiB frame cap is enforced before allocating (`codec.rs:56-63`).
- Pair and request timeouts are in place.
- Discovery is capped at 100 per page, 10 pages and 1,000 per peer.
- Remote session entries are validated field by field.
- The outbox caps are 1,000 rows or 8 MiB per peer and 32 MiB in total.
- The inbox cap is 100 pending per recipient.
- The token buckets hold one entry per peer and per recipient.

**Storage:**
- **Identity key:** created with `O_EXCL` and mode 0600, in a 0700 directory. A symlink is refused, and the key is never regenerated while peers exist.
- **`messages` rebuild, tested** on a database built from main's schema:
  - 2,000 rows came through byte-identical.
  - Four concurrent `init` runs were safe.
  - Four `kill -9` runs during a 400k-row rebuild each left the original table and FK intact, and a re-run then migrated it byte-identical. `integrity_check` passed.

**Settings writes:**
- They need the session and an exact Origin.
- `:harness` is matched against `Harness::ALL`, so there is no path traversal.
- The budgets path is fixed and its values are validated as finite numbers ≥ 0.
- The relay must be `default` or a parseable `https://` URL.
- Request bodies reject unknown keys.

**Test seams:** `AXON_FED_RELAY`, `AXON_FED_BIND` and `AXON_TEST_NOW_OFFSET_MS` are read only under `cfg!(debug_assertions)` (`fed/mod.rs:116-122`).

**Not covered:**
- No live two-node run was done for SEC-3, SEC-4, SEC-5 or SEC-11.
- The Windows ACL path was not checked.
- The Peers UI was not tested in a real browser.
