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
  - `200 {"token":"<43-char base64url>"}`, with `Set-Cookie: axon_session=<43-char
    base64url>; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000` (§12, C1).
  - `401 {"error":"sign_in"}` for a wrong, used or expired nonce. All three give the same
    body.
- Sessions are stored as hashes with `created_at`, `last_used_at` and `expires_at` (30 days),
  so they survive a restart.

**Guard.**

| Route | Auth |
|---|---|
| `GET /`, static UI assets, `manifest.webmanifest`, `sw.js`, `offline.html` | Public; they contain no data and no secret |
| `POST /api/session` | Loopback Host and matching Origin only |
| Every other `/api/*`, GET and SSE included | Cookie session and its token (`x-axon-session: <token>`; `?t=<token>` on `GET /api/stream`). Without both: `401 {"error":"sign_in","hint":"run axon open"}` |

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

## 12. Amendments

Where the build differs from sections 1–11, this section wins. Each entry says why. Entries
marked **open** are the owner's to accept or reverse; the rest are settled by the frozen
acceptance tests or by what the sections left unsaid.

**Pairing (§4).**
- The pair request frame also carries the joiner's proposed `generation`. The inviter takes
  the larger of the two so both sides agree, because `ping` compares generations for equality.
- The inviter learns the joiner's address from the connection's observed paths, never from
  claimed addresses. The dedup cache of observed addresses is cleared on each peer sync, so an
  address seen while the peer was `pending_confirm` is saved once it becomes `active`, and a
  restarted node can dial it again.
- `GET /api/fed` exposes `pair_code` on each peer in `pending_confirm`. §10's example omits it;
  §4 requires both screens to show it.
- `GET /api/fed` reports `enabled: false` whenever the service is not running.
- Creating invites, joining, confirming and sharing are in Settings (commit 92b5a23); the
  gap this entry once recorded is closed.

**Shares and the message path (§§6–8).**
- A share's two owners bump one shared `revision`, so changes can cross. A `share_update` with
  an equal revision and different flags is applied; an older one is ignored. A `share_update`
  that advances our revision is answered with our own flags at the new revision. Without this,
  one side's change could be lost permanently.
- Offered and active shares are resent on each peer (re)connect instead of tracking an
  acknowledged flag, because the schema is frozen (removed ones are not; see the fix round).
  Share frames are idempotent by revision.
- A reply is tied to the peer id and to the share the question arrived under (read from the
  `accepted` audit row), not to the discovery cache. The cache is only eventually consistent,
  and an answer must not depend on it.
- The sender writes `fed_audit` rows (direction `out`) for accepted, rejected and expired
  outcomes. §5 listed inbound decisions only.
- Hooks report their cwd, and a reported cwd replaces the stored one, so membership follows the
  agent when it moves.
- Delivery (§8) re-checks, at hook time, that the peer is `active` in the message's generation,
  that the share is still `active`, and that the recipient is still a member of the share's
  repository. A message that fails stays pending and is not deleted. An expired message is
  deleted from `messages` and audited as `expired`; its `fed_inbox` row stays, so a
  retransmission answers `duplicate`.
- A hook delivers at most 20 messages and 16 KiB of remote text per call. The rest wait.
- `msg::deliver` keeps a `from_id NOT LIKE 'peer:%'` filter on the local-message query: it is
  the partition between local and remote messages, not a skip. Remote rows go only through the
  remote framing.
- **Open.** A full inbox (100 or more pending for the recipient) is rejected with
  `recipient_full`, which is final: the sender's outbox marks the row rejected. A token-bucket
  limit stays `rate_limited`, which the sender retries. §8 step 8 named no reason for the full
  inbox.
- **Open.** `messages.from_id` no longer references `agents(id)`, because remote principals
  (`peer:<label>/<session>`) have no agents row (§5). Two triggers keep the check for every
  sender that is not `peer:`-prefixed. A database that still has the foreign key is rebuilt
  once, in a transaction, when it is opened. The receiver does not turn foreign keys off.
- Over-limit rejections are counted, not written (C3): a rejection past the rate limit, and
  any inbound rejection past 10 per peer per minute (tracked in memory), adds to
  `counters.rejected` through an in-memory counter and writes no audit row.
  The counter restarts at zero with the process.

**Lifecycle and health (§§9–10).**
- Pause, resume and remove each run as one transaction. The notices and the service reload are
  sent after the commit, with a 2 s budget, so removal never waits for an offline peer. §9 said
  `notice paused` is sent first; sent first it would block the action on the peer's network.
- `PUT /api/fed/peers/<id>/label` answers `400 {"error":"label_taken"}` when the label belongs
  to another non-removed peer.
- Remote lifecycle is visible and enforced (C2). A received `removed` notice ends the pairing
  as an owner removal would (shares end, queued messages are cancelled, pending inbound
  messages are deleted) and records `removed_reason='remote_removed'`; `DELETE
  /api/fed/peers/<id>` on such a peer is the owner's "forget": it answers 204 once and the peer
  stops being listed (`removed_reason='remote_removed_forgotten'`). A `paused` notice sets
  `peers.remote_paused`; our state is unchanged, sends are refused with `peer_paused`, queued
  rows wait, and we stop dialing that peer (it refuses us on purpose, so the closed connection is
  no fault: no `last_error`, state `offline`). A `resumed` notice, sent on resume with the same
  retries as a removal notice, clears the flag. `/api/fed` peers carry `remote_paused` and
  `removed_reason`. A `paused` or `resumed` notice lost to an outage is not resent: the other side
  keeps showing the peer paused (and sending refused) until a `resumed` notice or a re-pairing
  reaches it.
- `next_retry_at` is set when a dial attempt starts, to the time the attempt gives up
  (now plus the connect timeout), and cleared on connect. §10 left it empty during an
  in-flight dial.
- `counters` and `queue` come from the database: `queue` is the `queued` outbox rows; `sent_accepted`,
  `expired`, `rejected` and `cancelled` come from the outbox and `received` from `fed_inbox`.
- `/api/stream` emits `event: fed` on every service change and every 4 s, which satisfies "at
  least every 5 s".
- `GET /api/health` returns `{"status":"ok"}`. The frozen lifecycle suite polls it; no section
  defined it.

**Tests and tooling.**
- The single-instance lock test waits up to 1 s for the lock to release: forked children
  inherit the lock's file descriptor on macOS.
- The Peers panel was exercised with a stub DOM, not a browser. The checklist in
  `docs/FED-MANUAL.md` covers it by hand.

**Fix round (2026-10-02).** Contract changes and decisions made after review; the audit
reports are in `docs/audits/`.
- **C1, session token.** `POST /api/session` answers `200 {"token":"<43-char base64url>"}` and
  sets the cookie. The token is bound to the session (stored as `token_hash` next to
  `session_hash`; a database made before this gets the column by `ALTER`). The page keeps it in
  `localStorage`, which is origin-scoped, because browsers send the cookie to every port of
  127.0.0.1. Every `/api/*` request must carry `x-axon-session: <token>` that matches the
  cookie's session, else `401 {"error":"sign_in"}`; the check also covers the host binary's own
  routes. `GET /api/stream` takes `?t=<token>` (EventSource cannot set headers), read for that
  path only and never logged. An open stream is re-checked every 500 ms and ends when its session
  is revoked or expires.
- **Message path.** The outbox re-checks, in one write transaction right before each transmit,
  that federation is on, the peer is active in the row's generation, the share is active, the
  sender is still a member and `outbound`/`remote_inbound` are on; otherwise the row is
  cancelled with the reason, audited, and the sender gets a `sync` notice. A revision bumped
  since queueing is re-stamped, not a reason to drop. A peer's terminal rejection also tells
  the sender. Expiry, audit, notice and cancellation of a batch share one transaction. A
  revocation committed before the re-check ends the row; one committed after it finds the
  message already sent. A remote answer never satisfies a local blocking ask, which also
  requires the answerer to be the addressee of the question.
- **Durability.** Federation handlers whose commit precedes a wire ack (messages, shares,
  pairing and notices, admission) open the database with `synchronous=FULL`; everything else keeps
  NORMAL.
- **Rates and limits.** Every frame kind is rate-limited per peer and per kind (10/s, burst 20)
  before the handler touches the database; over it the peer gets `{"type":"error",
  "reason":"rate_limited"}`, which senders retry. The per-recipient limit stays in the message
  pipeline. Message stamps whose difference overflows are `bad_time`.
- **Shares.** Inbound messages record the share they were accepted through (`fed_inbox.share_id`),
  and unsharing deletes by it. Removed shares count toward a quota of 50 per peer
  (`too_many_shares`), are not resent on reconnect, and a retention sweep (at start, then
  hourly) keeps the newest 25 per peer. A removal frame lost to an outage is therefore not
  repeated; the peer learns when its next message is refused `unknown_share`.
- **Retention.** Outbox and inbox rows stay 90 days (the health counters read them, so that is
  also how far they count), audit rows 365 days, expired invites 7 days; expired dashboard
  sessions and the agent-session rows of trimmed shares go at each sweep.
- **Service.** The owner follows `fed_enabled` and `fed_relay` changes made by another process
  within 1 s. A failed read of the peers table closes every connection and admits nobody until
  the next 5 s tick. Dial tasks end with the service, and share frame handlers hold it weakly.
  A live label is unique (partial unique index); a join that loses the race fails `label_taken`.
- **Settings errors** answer a fixed `unavailable` and log the cause; `config.toml` budget edits are
  serialized and staged under unique names.

**Fix round 2 (2026-10-02).**
- **Transmit boundary (RR-1).** The service owns one gate (`transmit_gate`, a read-write lock).
  The outbox holds it shared from its authorization re-check until the frame is written; a local
  pause, remove, unshare or flag change takes it exclusively around its commit. So in the owning
  process no frame is written after such a commit returns, and no SQLite write transaction is held
  across a network await (the gate is released when the write finishes, not when the peer answers).
  The gate is global rather than per peer: a revocation may wait for in-flight writes to any peer,
  at most the 10 s request timeout. A revocation made by another process (the `axon bus` CLI) is
  seen by the owner's next re-check; the residual window is the time between that re-check and the
  frame's write, normally milliseconds and never more than the same 10 s.
- **Streams (RR-2).** An open `/api/stream` re-reads its session every 500 ms with a read-only query
  that must answer within 400 ms; a missing session, an error or a timeout closes the stream, so
  revoking or expiring a session ends it within 1 s even while a writer holds the database. The
  stream never refreshes `last_used_at`; only API requests do.
- **Lifecycle (RR-3, RR-4).** Each pairing carries `lifecycle_seq`, bumped by every pause, resume and
  removal of ours, and `remote_lifecycle_seq`, the newest of the peer's that we applied. `paused`,
  `resumed` and `state` notices carry `seq`; one that is not newer is ignored, so a late pause
  cannot undo a later resume. The acknowledgement carries the answerer's own `{paused, seq}`, which
  the sender learns. On every connection each side sends a `state` notice, so a notice lost while
  apart is repaired at the next connect. A peer that paused us is not dialed for traffic but is
  probed every 5 s (connect, trade `state`, close; silent when it refuses): a pause on both sides
  followed by a resume on both ends leaves two peers that reconnect without pairing again. Notices
  retry until acknowledged, an error answer included, except `unknown_peer` and
  `stale_generation`. A removal notice lost to an outage is still not repaired (see Shares above).
- **Reload (RR-5).** A failed read of the peers table and a failed reload worker both deny, close
  and retry at the next tick. **Retention (RR-7).** An old undelivered inbound message is deleted
  with its inbox record in the same transaction, never the record alone. **Dialers (RR-8).**
  Aborted dial tasks are reaped while running; shutdown signals the manager, which aborts and joins
  every dialer before it returns. **Budget file (RR-9).** Edits of `config.toml` hold an advisory
  file lock (`config.toml.lock`), so other `axon` processes are excluded too.
- **Admission (RR-11).** Before any address is persisted or a frame dispatched, a peer may open
  20 streams per second (burst 40) of any kind, built-in `ping` and unknown types included;
  over it the stream answers `rate_limited` and is counted like any other refusal. The per-kind
  rate of Rates and limits stays behind it.
- **Identity key (RR-14, SEC-12).** A key file readable by group or others is refused at start with
  the `chmod 600` fix; the mode is not repaired, because the key may already have been copied.
  A new key is created 0600 in a 0700 directory. Windows has no mode to read back, so there the
  owner-only ACL is still set rather than checked. **Refs (REL-13).** `axon bus guide` states that
  a ref uses `/` only.
- **Joiner label (SEC-6).** The inviter never stores the label the joiner chose; it names the
  pending peer `axon-` plus the 16 hex digits of the joiner's fingerprint (unique per node) until
  its owner renames it. The joiner still sends its label (it is validated, then ignored), and the
  inviter no longer answers `label_taken`. **Invite relay (SEC-7).** An invite whose `relay_url` is
  not `https://` is refused as `invalid_invite`; a private https host is not refused.
- **Startup (RR-16).** The root `axon` records its port, binds, and only then does the slower
  startup work (hooks, first snapshot, federation), so the port is open within milliseconds.
- **Not carried to pre-fix databases (RR-6, RR-10).** `lifecycle_seq`, `remote_lifecycle_seq` and
  the earlier fix-round columns are in the table definition only. Federation has never shipped
  (`job/fed` is unmerged), so no database outside development has `fed_inbox` rows or racing
  duplicate labels, and none needs an upgrade path.
- **Accepted, not fixed (P3).** Reasons are the residual risk, not a claim that it is absent.
  - BUG-11: thread names stay as sent on the wire; frozen tests assert it.
  - BUG-12: dial backoff already doubles from 1 s to 60 s with jitter. A message that fails to
    send retries after 1 s doubling to a fixed 10 s, without jitter; many queued rows to one peer
    retry together, which is harmless at the volumes this carries.
  - BUG-13: the outbox sends sequentially. After a failed send to a peer it skips that peer's other
    rows for the tick, so one unreachable peer costs at most one 10 s request timeout per tick; the
    messages to other peers are delayed by that much, not blocked.
  - SEC-8: the pair code is the owner's attestation that the digits matched, not something the
    server can verify; the manual says so.
  - SEC-9: the `peer:` partition keeps case-insensitive `LIKE` in the FK-replacement triggers and
    two filters. A local sender id spelled `PEER:x` skips the sender check and its messages leave
    local delivery. It is local-only (an agent registers its own ids), and fixing it means
    replacing triggers in `store.rs`, which is at its line limit.
  - SEC-10: four findings remain open (rustls 0.23.40 TLS 1.3 key-change handling, webbrowser
    1.2.1 URL handling, anyhow `downcast_mut`, and the unmaintained `paste`). Impact: the rustls
    transcript is still authenticated; the URL given to webbrowser is a fixed loopback link; the
    branch calls nothing unsound. A bump needs registry access and a rebuild of the dist size
    number. With `relay = "default"` the node's presence and home relay are published to n0's
    DNS and relays; the federation card in Settings should say so (UI, designer).
  - SEC-11: an unknown invite id now takes no write lock. Anyone who has seen an invite can still
    hold the 8 pairing slots for 15 s each, which blocks pairing (not messaging) while they do,
    and each wrong secret costs one write until the fifth kills the invite.
  - SEC-13(b): the pairing nonce is single-use and lives 60 s; it is visible in process arguments
    to processes of the same user, who can already read the database.
  - REL-5: the Windows ACL path has no test; CI builds and tests on `windows-latest` but the ACL
    assertion needs a Windows host to write and verify.
  - TEST-n: acceptance tests are Codex's; the implementer does not add or edit them.
