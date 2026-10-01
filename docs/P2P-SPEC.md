# SPEC: federation and Settings (contracts for tests and implementation)

Companion to `docs/P2P-PLAN.md`. The plan says *what* and *why*; this file fixes the
contracts that the acceptance tests (Codex) and the implementation (coders) must both
follow. Where this file and a council report disagree, this file wins. Change it only with
the owner's approval, and before the tests that depend on it.

## 0. Ground rules

- Toolchain: iroh 1.3 needs Rust 1.91, so the workspace `rust-version` moves from 1.86 to
  1.91, and so does the CI `msrv` job.
- **Federation is off by default.** Nothing listens and nothing dials until the owner turns
  it on in Settings. `axon bus doctor` reports `federation: off | on (<n> peers)`.
- Names:
  - The agent-facing verb `peers` keeps its meaning (who *I* can reach).
  - Remote principals are written `peer:<peer_label>/<session>`.
  - The human CLI namespace is `axon peer …` (optional, Settings is the primary surface).
- All new SQL and JSON is snake_case. Times are integer ms since the epoch (`_at` suffix);
  durations are ms (`_ms` suffix).
- **Test seams (debug builds only, `cfg(debug_assertions)`; the release binary ignores them):**

  | Variable | Effect |
  |---|---|
  | `AXON_FED_RELAY=disabled` | No relay, no discovery service |
  | `AXON_FED_BIND=127.0.0.1:0` | Bind the endpoint on loopback only |
  | `AXON_TEST_NOW_OFFSET_MS=<i64>` | Shifts the clock federation uses |

  Tests run two real `axon` processes, each with its own `XDG_DATA_HOME` and port, and talk
  to them over HTTP. No test reaches the internet.

## 1. Owner session (U0)

**Login link.**
- A nonce is 32 random bytes in base64url. A table holds its hash, `sha256` (see §5), with
  a lifetime of 60 s, and each nonce is single-use.
- `axon` writes a nonce on start, prints `Dashboard: http://127.0.0.1:<port>/#login=<nonce>`
  and opens the browser (unless `--no-open`).
- `axon open` does the same for a server that is already running. With `--print` it only
  prints, never opens.

**Exchange.**
- `POST /api/session` with `{ "nonce": "…" }`. It needs a loopback Host and an Origin that
  matches the dashboard; no token and no cookie.
- Responses:
  - `204`, with `Set-Cookie: axon_session=<43-char base64url>; HttpOnly; SameSite=Strict;
    Path=/; Max-Age=2592000`.
  - `401 {"error":"sign_in"}` for a wrong, used or expired nonce. All three give the same
    body.
- Sessions are stored as hashes with `created_at`, `last_used_at` and `expires_at` (30 days),
  so they survive a restart.

**Guard.**

| Route | Auth |
|---|---|
| `GET /`, static UI assets, `manifest.webmanifest`, `sw.js`, `offline.html` | Public; they contain no data and no secret |
| `POST /api/session` | Loopback Host and matching Origin only |
| Every other `/api/*`, GET and SSE included | Cookie session. Without one: `401 {"error":"sign_in","hint":"run axon open"}` |

- Every non-GET also keeps the Host and Origin checks.
- API responses carry `Cache-Control: no-store`.

**Removed.**
- The page token: `index.html` loses `<meta name="axon-token">`, `app.js` loses `token`, and
  the `x-axon-token` header is no longer accepted.
- The existing acceptance tests that use the token (`acceptance_audit_http`, `_ingest`,
  `m3`, `m4`, `m5`, `tests/common/http.rs`) are migrated to the login flow **by the test
  owner (Codex)**. This spec is the evidence for that change.

**UI.**
- With no session, the page shows a sign-in panel: "Run `axon open` in a terminal." It
  retries `/api/session` when the URL carries `#login=`, and removes the fragment with
  `history.replaceState`.
- Settings → Storage has "Sign out other browsers", which deletes every session except the
  current one.

**`/api/msg`.**
- It keeps its design (the operator sends along an existing edge, as the agent at the other
  end, BUS-PLAN §7), now behind the owner session.
- It refuses any `peer:` target: `400 {"error":"remote_target"}`. The operator does not
  message remote agents in v1.

## 2. Settings API (U1)

**`GET /api/settings`**
```json
{
  "capture":  { "enabled": true, "forced_off": false, "narrative_days": 7 },
  "usage":    { "retention_days": null },
  "budgets":  { "eur_per_day": null, "eur_per_week": null, "eur_per_month": null },
  "hooks":    [{ "harness": "claude", "installed": true, "config_path": "…" }],
  "storage":  { "db_bytes": 0, "wal_bytes": 0, "sessions": 1 },
  "federation": { "enabled": false, "relay": "default", "node_id": null, "fingerprint": null }
}
```
- `forced_off` is true when the server was started with `--no-content`.
- `hooks` lists every harness `install.rs` knows, with the state as `doctor` reports it.

**Writes.** Each returns the full new `GET /api/settings` body. Validation errors are
`400 {"error":"invalid","field":"…"}`.

| Route | Body | Effect |
|---|---|---|
| `PUT /api/settings/capture` | `{"enabled":bool,"narrative_days":1..365}` | Stored in `settings` (`capture_enabled`, `narrative_days`). Retention uses `narrative_days` in place of the fixed 7. |
| `PUT /api/settings/usage` | `{"retention_days":null\|1..3650}` | Stored in `settings`. The poller deletes usage rows older than the cut. `null` keeps them forever. |
| `PUT /api/settings/budgets` | Any of `eur_per_day`, `eur_per_week`, `eur_per_month` (number ≥ 0 or null) | Written into `~/.config/axon/config.toml` with `toml_edit`, keeping the other keys and comments. `src/config.rs` reads the same file, so there is one source. |
| `POST /api/settings/hooks/<harness>/install` and `/uninstall` | none | The existing `install.rs` / `uninstall.rs` code paths. Never another tool's entries. |
| `POST /api/settings/storage/compact` | none | `VACUUM`. `409 {"error":"busy"}` if the write lock cannot be taken within 1 s. |
| `POST /api/settings/sessions/revoke_others` | none | Deletes every session except the current one. |
| `PUT /api/settings/federation` | `{"enabled":bool}` or `{"relay":"default"\|"https://…"}` | See §3. |

## 3. Identity and the service (U2, U3)

**Identity.**
- An iroh `SecretKey` lives in `<data_dir>/fed/identity.key`. Directory 0700, file 0600,
  created exclusively (`O_EXCL`), with an owner-only ACL on Windows.
- The key is created only when federation is first enabled. A missing or corrupt key next
  to existing `peers` rows disables federation, with
  `last_error = "identity key missing or unreadable"`. It is never regenerated silently.
- The **fingerprint** of a node is the first 16 hex characters of `sha256(node_id bytes)`,
  shown as 4 groups of 4.
- The **pair code** shown at confirmation is the first 24 decimal digits derived from
  `sha256(min(node_a, node_b) ‖ max(node_a, node_b))`, shown as 6 groups of 4. Both screens
  show the same pair code.

**Service.**
- One service per data dir. `<data_dir>/fed/service.lock` is an OS advisory lock; a second
  `axon` on the same data dir does not start federation and logs who holds the lock.
- It is started by `src/main.rs` and by `serve::run` through the same `fed::service::start`.
- ALPNs: `axon/pair/1` for pairing and `axon/fed/1` for everything after.
- An inbound connection on `axon/fed/1` is closed **before reading a byte** unless its
  remote node id is a peer in state `active` or `pending_confirm`.
- Relay: `relay = "default"` uses iroh's default relays, or a URL for a self-hosted
  `iroh-relay`.

**Heartbeat.**
- A `ping` every 10 s.
- `rtt_ms` is an EWMA with α = 0.3 and is marked stale after 30 s.
- State becomes `offline` 30 s after the last authenticated response.
- Retries use jittered exponential backoff from 1 s, capped at 60 s.

## 4. Pairing (U4)

**Invite.**
- `POST /api/fed/invites` returns `{"invite_id","invite","expires_at"}`, where `invite` is
  `axon1:` + base64url(JSON):
  `{v:1, invite_id, node_id, relay_url|null, direct_addrs[], secret, expires_at, label}`.
- `secret` is 32 random bytes. The invite expires after 10 min. Only one invite is open at a
  time; a new one cancels the old one.
- `DELETE /api/fed/invites/<invite_id>` cancels.

**Join.**
- `POST /api/fed/join` with `{"invite":"axon1:…","label":"alice"}`. The label matches
  `[A-Za-z0-9._-]{1,32}` and is unique among this side's non-removed peers.
- The joiner dials `node_id` on `axon/pair/1`. iroh authenticates that key, so the inviter's
  pin is verified before anything is sent.
- The joiner then sends `{invite_id, secret, label}`.
- The inviter checks the invite against these rules:
  - It must exist, must not have expired, and must not have been consumed.
  - The secret is compared in constant time.
  - A wrong secret counts as an attempt; after 5 attempts the invite is dead.
  - The first valid joiner node consumes the invite. Another node then gets the same
    `invalid_invite` error.
  - A retry by the same joiner node returns the same pending pairing.
- Both sides then create a `peers` row in state `pending_confirm` with a new `generation`,
  and show the fingerprints and the pair code.

**Confirmation.**
- `POST /api/fed/peers/<peer_id>/confirm` with `{"pair_code":"…"}`. It must equal the
  computed code, or the response is `400 {"error":"pair_code_mismatch"}` and the peer is
  removed.
- Each side sends a `confirmed` frame. A peer becomes `active` only when its local confirm
  *and* the remote `confirmed` frame have both happened, within 10 min. Otherwise it becomes
  `removed` with the reason `confirm_timeout`.
- `POST /api/fed/peers/<peer_id>/reject` removes it at once.

**After pairing.** A new peer has zero shares, so discovery and messages are denied.

Every invite error (wrong, expired, consumed, too many attempts) gives the same
`400 {"error":"invalid_invite"}`.

## 5. Schema (U2), additive in `store.rs`

```sql
CREATE TABLE IF NOT EXISTS dashboard_sessions (session_hash TEXT PRIMARY KEY, created_at INTEGER NOT NULL, last_used_at INTEGER NOT NULL, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS login_nonces (nonce_hash TEXT PRIMARY KEY, expires_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS peers (
  peer_id TEXT PRIMARY KEY,            -- random, local
  node_id TEXT NOT NULL,               -- remote iroh public key
  label TEXT NOT NULL,
  generation INTEGER NOT NULL,         -- new on every pairing of this node_id
  state TEXT NOT NULL CHECK (state IN ('pending_confirm','active','paused','removed')),
  local_confirmed_at INTEGER, remote_confirmed_at INTEGER,
  paired_at INTEGER NOT NULL, paused_at INTEGER, removed_at INTEGER, removed_reason TEXT,
  last_error TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS peers_live_node ON peers(node_id) WHERE state <> 'removed';
-- Last direct addresses seen on an authenticated connection from this peer (never a claim the
-- peer makes about itself); fed to the endpoint on start so peers re-find each other after a
-- restart without a relay. Deleted with the peer on remove. Amendment 2026-10-01 (U4 finding).
CREATE TABLE IF NOT EXISTS peer_addrs (peer_id TEXT PRIMARY KEY, addrs_json TEXT NOT NULL, seen_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS peer_invites (invite_id TEXT PRIMARY KEY, secret_hash TEXT NOT NULL, expires_at INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, consumed_by TEXT, cancelled_at INTEGER);
CREATE TABLE IF NOT EXISTS peer_shares (
  share_id TEXT PRIMARY KEY, peer_id TEXT NOT NULL, label TEXT NOT NULL,
  local_repo TEXT,                     -- canonical repo_of path; NULL until this owner maps it
  inbound INTEGER NOT NULL, outbound INTEGER NOT NULL,
  remote_inbound INTEGER NOT NULL DEFAULT 0, remote_outbound INTEGER NOT NULL DEFAULT 0,
  revision INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('offered_out','offered_in','active','removed')),
  root_commit TEXT                     -- matching hint only
);
CREATE TABLE IF NOT EXISTS fed_sessions (session TEXT PRIMARY KEY, agent_id TEXT NOT NULL, share_id TEXT NOT NULL, UNIQUE(agent_id, share_id));
CREATE TABLE IF NOT EXISTS fed_outbox (
  message_id TEXT PRIMARY KEY, peer_id TEXT NOT NULL, generation INTEGER NOT NULL,
  share_id TEXT NOT NULL, revision INTEGER NOT NULL, from_agent TEXT NOT NULL,
  envelope_json TEXT NOT NULL, bytes INTEGER NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('queued','accepted','expired','cancelled','rejected')),
  attempts INTEGER NOT NULL DEFAULT 0, next_attempt_at INTEGER, last_error TEXT
);
CREATE TABLE IF NOT EXISTS fed_inbox (
  peer_id TEXT NOT NULL, generation INTEGER NOT NULL, message_id TEXT NOT NULL,
  content_hash TEXT NOT NULL, local_message_id TEXT NOT NULL, accepted_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  PRIMARY KEY (peer_id, generation, message_id)
);
CREATE TABLE IF NOT EXISTS fed_remote_sessions (peer_id TEXT NOT NULL, share_id TEXT NOT NULL, session TEXT NOT NULL, label TEXT NOT NULL, availability TEXT NOT NULL, seen_at INTEGER NOT NULL, PRIMARY KEY (peer_id, session));
CREATE TABLE IF NOT EXISTS fed_audit (seq INTEGER PRIMARY KEY, ts INTEGER NOT NULL, peer_fingerprint TEXT, generation INTEGER, share_id TEXT, message_id TEXT, direction TEXT, decision TEXT NOT NULL, reason TEXT);
```

- **Inbound messages** are stored as ordinary `messages` rows, with
  `from_id = 'peer:<label>/<session>'` frozen at receipt, and are linked through
  `fed_inbox.local_message_id`. No `agents` rows and no `edges` rows are created for remote
  principals.
- **Audit.** Every `fed_audit` insert also appends an `events` row (actor `fed`, verb =
  `decision`, payload hash over the `fed_audit` row), in the same transaction. Neither
  table ever holds a secret, a key or a message body.
- **Durability.** Federation writes use `PRAGMA synchronous=FULL` for the transaction that
  precedes a network acknowledgement. Hook transactions keep `NORMAL`.

## 6. Shares (U5)

**Offer.**
- `POST /api/fed/peers/<peer_id>/shares` with `{"local_repo","label","inbound","outbound"}`.
  `local_repo` must be a repo `repo_of` resolves from an agent cwd or a path the owner
  picks; otherwise `400`.
- The share is created as `offered_out`, and a `share_offer` frame carries
  `{share_id, label, revision, inbound, outbound, root_commit}`.

**Receiving an offer.**
- The other side records it as `offered_in`, with `local_repo = NULL`.
- Settings suggests the local repo whose root commit equals `root_commit`; it is only a
  suggestion.
- `POST /api/fed/shares/<share_id>/accept` with `{"local_repo","inbound","outbound"}`. The
  share becomes `active` on both sides once the accept frame arrives.

**Changes.**
- `PUT /api/fed/shares/<share_id>` with `{inbound, outbound}`.
- `DELETE /api/fed/shares/<share_id>` sets state `removed`.
- Every change bumps `revision` and sends `share_update` or `share_remove`.
- An unshare cancels queued outbox rows for that share, and deletes undelivered inbound
  rows for that share, in the same transaction.

**Membership.**
- A local agent belongs to share S iff `repo_of(agent.cwd)` (resolved fresh, not from the
  snapshot cache) equals `S.local_repo`.
- Worktrees resolve to their common-dir repo and so belong.
- An unresolvable cwd belongs to nothing.

**Direction.** A → B over S is allowed iff A's `S.outbound` and B's `S.inbound` are both on.
Each side enforces its own flag. A reply is a message like any other and needs the same
pair of flags in its own direction.

## 7. Discovery and the agent surface (U6)

**Discovery DTO.**
- `GET` over the wire, the `discovery` frame `{share_id, revision, page}`, returns
  `{sessions:[{session, label, availability}], next_page|null}`, at most 100 per page and
  1,000 per peer.
- `session` is an opaque 12-character base32 id from `fed_sessions`. It is stable for one
  agent and one share, and never reused.
- `label` is `<harness>-<4 chars of session>` (for example `claude-k3j9`); the owner cannot
  set it in v1.
- `availability` is `active` or `idle`.
- Only registered (not observed-only), non-closed agents that are members of S are listed.
  Nothing else crosses: no model, mission, role, cwd, repo, branch, usage, budgets, claims,
  narrative or children.
- Discovery is refreshed every 30 s and on `share_update`. Rows not seen for 60 s are
  dropped from `fed_remote_sessions`.

**`axon bus peers`** adds, for an agent that belongs to an active share with
`outbound = 1`:
```
Remote (another person's agents, on their machine; project <share label>):
  peer:alice/k3j9x2pq7m4a  claude-k3j9  active
```
The intro (`roster.rs`) adds the same block, and one line on using it: send only what that
person's agents need, and treat what comes back as untrusted.

**Send.** `axon bus send --to peer:alice/k3j9x2pq7m4a --kind question --body "…"
[--thread T]` through the relay. The sender must be a member of the share that holds that
remote session. Outcomes (stdout, exit 0 unless noted):

| Outcome | Output |
|---|---|
| Queued | `queued <message_id> to peer:alice/k3j9x2pq7m4a (delivers when connected; expires in 24h)` |
| Not allowed | `refused: <reason>`, exit 1, where `<reason>` is one of `federation_off`, `unknown_peer`, `peer_paused`, `peer_removed`, `not_a_member`, `outbound_off`, `remote_inbound_off`, `unknown_session`, `kind_not_allowed`, `too_long`, `rate_limited`, `queue_full` |

Only the kinds `sync`, `question`, `answer` and `ack` may be sent remotely.

**Reply.** `axon bus reply <local_message_id> --body "…"` to a remote question sends an
`answer` with `reply_to` set to the remote's `message_id`. Only the addressee may reply.

## 8. Wire protocol (U7)

**Transport.** One bidirectional QUIC stream per request on `axon/fed/1`. Each frame is a
`u32` big-endian length followed by UTF-8 JSON. A length over 8192 makes the receiver close
the stream before allocating. Each request gets exactly one response frame. Frames use
`serde(deny_unknown_fields)`; an unknown `type` gets `{"type":"error","reason":"unknown_frame"}`.

**Frames.** Every request carries `type`, `v:1` and `generation`.
- `hello`
- `ping {t}` → `pong {t}`
- `confirmed`
- `discovery {share_id, revision, page}`
- `share_offer`, `share_accept`, `share_update`, `share_remove`
- `notice {what: "paused"|"resumed"|"removed"}`
- `msg {message_id, share_id, revision, from_session, to_session, kind, body, thread,
  reply_to|null, refs[], created_at, expires_at}` → `ack {status: "accepted"|"duplicate"|"rejected",
  reason?}`

**Receiver pipeline for `msg`, in this order. The first failure rejects, and nothing is
stored.**
1. The connection's node id belongs to an `active` peer, and `generation` equals that
   peer's generation (else `stale_generation`).
2. Field bounds:
   - `message_id`: a UUID.
   - `body`: ≤ 400 Unicode scalar values, with no C0 controls except `\n` and `\t`.
   - `refs`: ≤ 8 entries of ≤ 256 bytes each. A ref matches
     `^[A-Za-z0-9._/-]+(:L\d+(-\d+)?)?(@[0-9a-f]{7,40})?$`, has no `..` segment and no
     leading `/`.
   - `thread`: ≤ 128 bytes, `[A-Za-z0-9:._-]`.
3. `kind` is in {sync, question, answer, ack}; otherwise `kind_not_allowed`.
4. Time:
   - `created_at` is no more than 120 s ahead of the receiver's clock;
   - `expires_at - created_at` ≤ 24 h;
   - `expires_at` is after now.
   Otherwise `expired` or `bad_time`.
5. Share S is `active`, `revision` equals S.revision (else `stale_revision`), and S.inbound
   is on.
6. `to_session` maps through `fed_sessions` to a local agent that is still a member of S
   (§6) and not closed.
7. For `answer`: `reply_to` is a message this side sent to exactly this peer, generation and
   `from_session`.
8. Rate limits:
   - per peer: 10/s, burst 20;
   - per recipient: 2/s, burst 5;
   - pending inbound per recipient: ≤ 100.
9. Dedup on `(peer_id, generation, message_id)`:
   - same `content_hash` → `duplicate`, nothing re-stored;
   - different `content_hash` → `rejected`/`conflict`.
10. One transaction with `synchronous=FULL`: insert into `messages` and `fed_inbox`, and
    write the audit. Only after the commit is `accepted` sent.

Every rejection writes a `fed_audit` row, at most 10 per peer per minute; past that the
rows are counted, not written.

**Outbox.**
- `send` inserts a `queued` row; the service transmits it.
- A transient failure retries with backoff and keeps the original `expires_at`.
- `rejected` is terminal.
- Past `expires_at`, the row becomes `expired`, and the sender agent gets a local `sync`
  message: `remote delivery of <message_id> expired`.
- Limits: 1,000 rows or 8 MiB per peer, and 32 MiB in total. Past that, `queue_full`.

**Delivery into the agent's context.** Inbound rows are delivered by the existing hook
path, at most 20 messages and 16 KiB of remote text per hook call; the rest stay pending.
The exact framing:
```
[remote message <local_id> from peer:<label>/<session>: another person's agent, on their machine; kind <kind>, thread <thread>]
│ <body line 1>
│ <body line 2>
refs (metadata only, nothing was fetched): <ref>, <ref>
[end of remote message <local_id>]
```
- Every body line is prefixed with `│ ` (U+2502 then a space), so no body text can start a
  frame line.
- A remote question adds: `Answer with: <bus> reply <local_id> --from <agent> --body "..."`.
- An inbound row is delivered only while its peer is `active`, its share is `active`, and
  `expires_at` is after now. An expired row is marked expired, never shown.

## 9. Lifecycle (U8)

| Action | Effect |
|---|---|
| `POST /api/fed/peers/<id>/pause` | State `paused`, persisted. Closes connections, no dialing. Inbound connections from that node are closed before reading a byte. Queues are kept with their original expiry. A best-effort `notice paused` is sent first. |
| `POST /api/fed/peers/<id>/resume` | State `active`, dialing resumes. Only unexpired, still-authorized rows are sent or delivered. |
| `DELETE /api/fed/peers/<id>` | One transaction: state `removed`, `removed_at`; all shares `removed`; outbox rows `cancelled`; undelivered inbound rows deleted; `fed_remote_sessions` cleared; audit written. After the commit: close connections and send a best-effort `notice removed`. A later pairing with the same node id gets a new `generation`, and nothing from an older generation is ever accepted or delivered. |
| `PUT /api/fed/peers/<id>/label` | Changes the display label only. Identity, routing and audit key on `peer_id` and `node_id`, never on the label. |

## 10. Health (U9)

**`GET /api/fed`**
```json
{
  "enabled": true, "node_id": "…", "fingerprint": "abcd ef01 2345 6789", "relay": "default",
  "invites": [{ "invite_id": "…", "expires_at": 0 }],
  "peers": [{
    "peer_id": "…", "label": "alice", "fingerprint": "…",
    "state": "pending_confirm|connected|reconnecting|offline|paused|removed|incompatible",
    "path": "direct|relay|none",
    "last_handshake_at": 0, "heartbeat_age_ms": 0, "rtt_ms": 42, "rtt_stale": false,
    "last_error": null, "next_retry_at": null,
    "queue": { "count": 0, "bytes": 0, "oldest_at": null },
    "counters": { "sent_accepted": 0, "received": 0, "expired": 0, "rejected": 0, "cancelled": 0 },
    "shares": [{ "share_id": "…", "label": "…", "local_repo": "…", "state": "active",
                 "inbound": true, "outbound": true, "remote_inbound": true, "remote_outbound": true }]
  }]
}
```
- The live states are derived from the service's memory. The stored state is the floor:
  `paused` and `removed` always win.
- Counters count unique messages; retries never add to them.
- `/api/stream` emits an `event: fed` with this body on every change, and at least every 5 s
  while any peer is `active`, so ages stay current without database writes.

## 11. Acceptance mapping

The rows A01–A32 in `docs/reviews/P2P-COUNCIL-gpt.md` §3, plus A33, A34 and S1–S5 in
`docs/P2P-PLAN.md`, are tested against *this* contract. They fall into two groups:
- **Automated:** every row except those listed next, run with two processes on loopback
  under the §0 seams.
- **Manual, checked by the security review and recorded in the release notes:**
  - A29, other-account access: only the 0600/0700 modes and the refusal on a failed chmod
    are automated.
  - A30, power loss.
  - The relayed path across two real networks.
