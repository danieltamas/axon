# PLAN: Axon federation (two machines, one shared project)

Status: **draft for the owner's go-ahead** (2026-10-01). Mode: High-risk (auth and a new
network boundary). Inputs: `docs/reviews/P2P-COUNCIL-BRIEF.md` and the council reports in
`docs/reviews/P2P-COUNCIL-{gpt,nemotron,longcat}.md`. The council was degraded: 3 of 5
seats reported (GPT, Nemotron 3 Ultra, LongCat 2.5); Muse, Ling and MiMo returned nothing.
The GPT report is the most complete, and its main code claims were checked against the
source (`serve.rs:219`, `serve.rs:314`, `msg.rs:459`, `src/server.rs:65`).

## 1. Goal and what "complete" means

Two developers each run Axon. From each dashboard's **Settings** page they pair the two
Axons, share one project, and watch the connection's health. Then agents in that project
on either machine can message each other, and either owner can **disconnect** (pause) or
**remove** (revoke) the peer at any time.

**Complete when:** two Axons on two machines on different networks (no shared LAN, no VPN) pair, share project P, exchange question/answer messages between an agent in P on
each side, show live health, then pause and remove. All of it is done from Settings, and
the frozen acceptance suite `cargo test --test acceptance_fed` passes on macOS, Linux and
Windows CI.

## 2. Council verdicts and what this plan decided

| Question | Council | Decision |
|---|---|---|
| SEC-7: other local accounts can read the page token | Unanimous that federation makes it critical. GPT: authenticate the owner on the whole dashboard. Nemotron: move pairing to the CLI. LongCat: a mandatory fingerprint check is enough. | **GPT's fix (U0).** It is the only one that keeps pairing in Settings, as the owner asked, and it also closes the existing `/api/msg` sender spoof. A mandatory fingerprint confirmation is added on top. |
| Transport | Unanimous: B, mTLS over a network both machines reach (LAN or Tailscale); iroh only if that prerequisite fails; no hosted relay. | **Revised by the owner (2026-10-01): iroh.** The machines share no LAN, and requiring Tailscale would slow adoption, which is exactly the council's condition for iroh. iroh is QUIC + TLS 1.3 where the node id *is* the Ed25519 key, so pinning is built in. It hole-punches direct connections and falls back to a relay that forwards end-to-end-encrypted traffic. Default: n0's public relays. Settings takes a self-hosted relay URL for teams that do not want a third party to see connection metadata. Fixed ALPN plus version, no 0-RTT, fail closed on a mismatch. |
| Pairing | GPT: an invitation blob with high entropy, so no PAKE is needed (B needs an endpoint in the invite anyway). Others: CPace. | **An invitation blob** containing the endpoint, the inviter's key pin, a 256-bit secret, an expiry of 10 minutes, and the protocol version. It is pasted into Join. Both owners then confirm the fingerprints before the pairing becomes active. It is single-use, and at most 5 failed attempts are allowed. |
| Project key | GPT: an opaque share id, mapped to a local repo explicitly by each owner. LongCat: a hash over the remote and HEAD. Nemotron: a hash of the local path. | **GPT's.** A HEAD hash changes on every commit, and a path differs across machines. The root-commit hash is only a *hint* to pre-select the local mapping. A remote URL never authorizes. |
| Remote message kinds | GPT: sync, question, answer, ack. Nemotron: drop sync and allow handoff. | **sync, question, answer, ack.** Handoff, redirect, stop, and the claim, budget, link and grant verbs are rejected on receive. |
| Disconnect vs Remove | Unanimous: keep them distinct, and make pause durable. | **Paused** (persists across restarts, no reconnect, queues frozen, Resume) and **Removed** (revoked at once, queues cancelled, a tombstone kept, re-pairing starts from zero grants). |
| Where it runs | Unanimous: a module of `axon-bus`, owned by the long-running server. | **`crates/axon-bus/src/fed/`**, started once per data directory under a singleton lock. Hooks and the CLI only enqueue. |
| Outbox TTL | GPT: 24 h, kept on retry and enforced again on delivery. Nemotron: 7 days, configurable. | **24 h**: a question older than that is stale. It is reported as *expired* to the sender, never silently dropped. |

## 3. Layout: units, dependencies, layers touched

Order is by dependency. Each unit names the layers it touches:
schema → bus logic → API → UI → agent text (intro/guide) → docs → tests.

**U0. Owner session on the dashboard** (prerequisite; no dependencies).
- `axon` opens the browser on `/#login=<nonce>`. The nonce is single-use, lasts 60 s, and is
  written 0600 to the data dir. The page exchanges it at `POST /api/session` for an
  `HttpOnly; SameSite=Strict` cookie.
- Every `/api/*` read, the SSE stream and every write requires that session. The page token
  meta is removed.
- `/api/msg` keeps the operator-as-edge design (BUS-PLAN §7), now behind the owner session, and refuses `peer:` targets.
- `axon open` issues a fresh link.
- Layers: `serve.rs` guard, `app.js` bootstrap, `src/main.rs` open, README, and the BUS-PLAN §7 SEC-7 entry.

**U1. Settings view** (depends on U0).
- A new `#/settings` route and nav tab. Its sections (the owner chose the full scope,
  2026-10-01):

  | Section | Holds |
  |---|---|
  | Peers | Everything in U4–U9 |
  | Capture | Content capture on or off (today `--no-content`), and narrative retention in days (today fixed at 7) |
  | Usage | Usage retention: keep forever (today's behaviour) or N days |
  | Budgets | Daily, weekly and monthly EUR budgets, the same values `src/config.rs` reads today |
  | Hooks | Each harness's install state as `doctor` sees it, with Install and Uninstall |
  | Storage | Database size and a Compact action (`VACUUM`, refused while the database is busy) |
- **One source per setting.** Budgets stay in the existing config file. The bus keys live in
  the existing `settings` table. No setting is stored in two places, and the CLI and the page
  read the same value.
- Every change is applied by the server and confirmed back; no optimistic UI.
- Layers: `app.js`, new `settings.js` and `settings-api` handlers (`fed/` handlers for
  Peers), `style.css`, the `assets.rs` table, `transcript.rs` retention, `config.rs`,
  `install.rs`/`doctor.rs` (reused, never duplicated) and README.
- Acceptance (in addition to A32/A34):
  - **S1.** Each setting changed in the page is visible to the CLI and `doctor`, and persists
    across a restart.
  - **S2.** A retention cut deletes only rows older than the cut.
  - **S3.** Install and Uninstall touch only Axon's own hook entries, never another tool's.
    Tests use a temp `HOME`, never the real config.
  - **S4.** Compact while a hook holds the write lock fails with a visible message, and no data
    changes.
  - **S5.** A budget value written in the page equals what `src/config.rs` loads.

**U2. Identity and schema** (depends on none).
- The Ed25519 key is created exclusively, owner-only (an ACL on Windows), never
  regenerated silently, and federation is refused if it cannot be protected.
- New tables (snake_case, additive `CREATE TABLE IF NOT EXISTS`):

  | Table | Holds |
  |---|---|
  | `peers` | `peer_id`, `pubkey`, `label`, `endpoint`, `state`, `generation`, `paired_at`, `paused_at`, `revoked_at` |
  | `peer_invites` | pending invitations, attempts, expiry |
  | `peer_shares` | `share_id`, `peer_id`, `local_repo`, `inbound`, `outbound`, `revision` |
  | `fed_outbox` | outbound messages and their delivery state |
  | `fed_seen` | dedup by `(peer_id, generation, message_id)` |
  | `fed_audit` | structured, linked into the existing `events` chain |
- Layers: `store.rs` schema, a new `fed/identity.rs`.

**U3. Transport service** (depends on U2).
- An iroh endpoint whose secret key is the U2 identity. Connections are accepted only from pinned node ids, and anything else is closed before any application byte is read. Fixed ALPN `axon/fed/1`, no 0-RTT.
- The relay URL comes from Settings: the n0 default, or self-hosted (`iroh-relay`).
- Health records whether the path is direct or relayed.
- One service per data dir (an OS lock); a second server reports the owner instead of starting.
- 10 s heartbeat, offline after 30 s, jittered backoff capped at 60 s.
- The listener exposes only the federation protocol, never the dashboard.
- Layers: `fed/transport.rs` and `fed/service.rs`, started from `src/main.rs` and `serve::run`.

**U4. Pairing** (depends on U0–U3).
- Invite → Join → both confirm the fingerprint → paired with zero grants.
- The states are pending, confirmed, expired and consumed. The secret is disclosed only
  after the inviter's pin is verified.
- Layers: `fed/pairing.rs`, `/api/fed/*`, `settings.js`.

**U5. Project shares** (depends on U4).
- An owner offers a share id. The other owner maps it to a local repo; the root-commit hint
  pre-selects a candidate.
- Inbound and outbound are granted separately. A reply needs the reverse grant.
- Every unshare or re-share bumps `revision`.
- An agent's project membership is rechecked at enqueue, accept and delivery. Unknown
  membership fails closed.
- Layers: `fed/shares.rs`, `snapshot.rs` (`repo_of` reused), the API, `settings.js`.

**U6. Remote principals and discovery** (depends on U5).
- Remote sessions are typed principals: `(peer key, generation, session)`. They are never
  rows in `agents` and never tree, link or grant edges.
- Discovery uses an allowlisted DTO: opaque session id, label, availability and share. It
  carries no model, mission, path, branch, usage, narrative or children, and no
  observed-only sessions.
- `peers` and the intro list remote sessions in shared projects as `<peer>/<session>`.
- Layers: `fed/discovery.rs`, `roster.rs`, `guide.rs`.

**U7. Messages over the wire** (depends on U6).
- The envelope carries: version, message id, generation, share id and revision, source and
  target session, thread mapping, created and expires, body (≤ 400 chars), and at most 8
  inert refs.
- Receive-side kind allowlist. Dedup before acknowledging, and a durable commit before the
  ack.
- An outbox with stable ids and its states: queued, peer-accepted, hook-prepared, answered,
  expired, cancelled, rejected.
- Limits (frozen before tests):

  | Limit | Value |
  |---|---|
  | Frame | 8 KiB |
  | Rate per peer | 10 messages/s, burst 20 |
  | Rate per recipient | 2 messages/s, burst 5 |
  | Queue per peer | 1,000 messages / 8 MiB |
  | Queue, all peers | 32 MiB |
  | Per hook injection | at most 20 messages / 16 KiB |
- Remote text is escaped and framed as from `<peer label>`'s agent on another machine.
- `send --to <peer>/<session>` enqueues locally and returns "queued".
- Layers: `fed/wire.rs` and `fed/outbox.rs`, plus `msg.rs` delivery, which only filters
  and labels: no new logic there, since `msg.rs` is at 496 lines.

**U8. Lifecycle** (depends on U7).
- Pause and Resume are durable.
- Remove revokes locally and atomically: it closes sessions, cancels queues and clears
  discovery, then sends a best-effort notice, and never waits for that notice.
- Re-pairing creates a new generation with zero grants. Messages from an old generation
  are never delivered.
- Layers: `fed/service.rs`, the API, `settings.js`.

**U9. Health in Settings** (depends on U3, U7, U8).
- **Peer state:** pending, connected, reconnecting, offline, paused, revoked, or
  incompatible.
- **Per peer:** last handshake, heartbeat age, RTT (marked stale after 30 s), last error
  and the next retry, the queue (count, bytes, oldest), the accepted, received, expired and
  rejected counters, and the grant directions per share.
- **Live updates:** pushed over SSE from the service's state, not from database changes.
- Layers: `fed/service.rs` → `/api/stream`, and `settings.js`.

**U10. Docs and release.** README, `guide.rs` (remote collaborators, never approval),
BUS-PLAN §2 and §7 (U0 replaces SEC-7), and the release notes.

## 4. How it is measured

1. **Acceptance:** `cargo test -p axon-bus --test 'acceptance_fed_*'` on the 3-OS CI matrix. Two isolated
   data dirs, controlled keys, clocks and network faults, real hook payloads.
2. **End-to-end:** `scripts/fed-e2e.sh` runs two `axon` instances on one machine (different
   ports and data dirs). It drives pair → share → question → answer → pause → resume →
   remove through the HTTP API, and exits 0. Then the same steps run by hand across two
   machines on different home networks, with screenshots of Settings in each state. A
   run with UDP blocked must show the relayed path working.
3. **Speed:**
   - The hook stays ≤ 10 ms p95 with an empty inbox and ≤ 40 ms p95 when delivering
     (hyperfine, as today), so federation adds no work to the hook path.
   - From send to the remote inbox commit: ≤ 1 s p95 on a direct path, ≤ 2 s relayed.
   - Health turns offline ≤ 30 s after a blackhole.
4. **Size:** the binary stays ≤ 15 MB. iroh adds dependencies: measure after U3, and raising the limit is the owner's call.

## 5. Acceptance criteria (a different vendor turns these into tests first)

The test contract is `docs/reviews/P2P-COUNCIL-gpt.md` §3, rows **A01–A32**, adopted as
written, with the limits in U7 frozen. It adds:

- **A33 (LongCat AC8).** An owner who rejects the fingerprint at
  confirmation leaves both sides with no peer row and the invitation consumed.
- **A34 (user requirement).** Every lifecycle action is available only from Settings: no
  CLI step is needed for invite, join, confirm, share, pause, resume or remove. A CLI
  equivalent (`axon peer …`) may exist, but nothing depends on it.

Per unit:

| Unit | Rows |
|---|---|
| U0 | A01 |
| U1 | A32, A34 |
| U2 | A29, A30 |
| U3 | A02, A06, A26, A27, A28 |
| U4 | A03, A04, A05, A33 |
| U5 | A07, A08, A09 |
| U6 | A10, A13, A21, A22 |
| U7 | A11, A12, A14, A15, A16, A17, A23, A24, A25 |
| U8 | A18, A19, A20 |
| U9 | A28, A32 |
| Audit (all units) | A31 |

## 6. Quality criteria

- **Code:**
  - Each `fed/*` file ≤ 500 lines.
  - English, snake_case in SQL and JSON, matching the existing tables.
  - Parameterized SQL, clippy `-D warnings`.
  - No new message logic in `msg.rs` beyond the delivery filter.
  - No second HTTP server for the dashboard; one federation service per data dir.
- **Security**, one check per trust boundary:

  | Boundary | Check |
  |---|---|
  | Dashboard | Owner session (U0) |
  | Network listener | mTLS with pinned keys, and only the federation protocol |
  | Inbound frames | Size cap before decoding, field bounds, kind allowlist, share, revision and membership check, dedup, expiry |
  | Agent context | Escaped framing that names the remote human; never a command or permission |
  | Identity key | Owner-only, fails closed |
  | Audit | Peer fingerprint and decision on every accept, reject and policy change; no secrets or bodies |

  Every failure branch (timeout, a corrupt key, a busy DB, an unknown version) denies.
- **Speed:** as in §4. No network wait inside a SQLite write transaction or a hook.

## 7. Execution (owner decision, 2026-10-01)

Contracts: `docs/P2P-SPEC.md`. The tests and the code follow it, not this plan's prose.

- **Branch:** `job/fed` off `main`. Each unit goes on `job/fed/<unit>` in its own worktree, and the orchestrator merges it into `job/fed`.
- **Orchestrator:** this session (Opus). It dispatches units, merges, runs the frozen suite, and writes no `src` itself.
- **Tests first:** Codex writes `crates/axon-bus/tests/acceptance_fed*.rs` and `acceptance_settings.rs` from the spec in one pass, and migrates the token-based tests to the login flow (spec §1). From then on the tests are frozen: a red test is fixed in `src`.
- **Coders:** Sonnet 5.5 coder agents, one per unit, each with coder-playbook and verify. A unit is done when its rows pass, clippy `-D warnings` is clean and every file is ≤ 500 lines.
  - Order: U0 and U2 run in parallel, since they touch different files. Then U1 and U3, then U4 → U8 in sequence, then U9 and U10.
  - The Settings UI is built by a designer agent on Sonnet 5.5 with the impeccable pipeline.
- **Review team, at the end, in parallel:**
  - QA: the end-to-end script and coverage gaps against the spec.
  - Tester: the full suite on the 3 CI OSes, plus the two-machine manual run.
  - Security: the post-ship audit with a threat model of the network and auth boundaries.
  - Codex: a cross-vendor review of the whole `job/fed` diff.

  Then one bounded fix round by a Sonnet coder. The release needs a clean security sign-off.

## 8. Open, for the owner

Decided on 2026-10-01:
1. **Transport is iroh.** There is no shared LAN, and a Tailscale requirement would slow
   adoption.
2. **Settings ships in full** (U1).

Still open:
3. **U0 changes how the dashboard opens.** Opening `127.0.0.1:7777` directly shows a sign-in
   prompt that tells you to run `axon open`, and bookmarks keep working while the cookie
   lives. This fixes SEC-7 for everything, not only federation.
