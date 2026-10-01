# Council brief: Axon peer-to-peer federation (design review, 2026-10-01)

You are a skeptical reviewer of a **proposed design**, not of code. Nothing below is built.
Ground every point in this repository (file:line) where the design touches existing code,
and say plainly when a proposal is wrong, unsafe or overbuilt. Do not run `cargo build` or
`cargo test`.

## What exists today (read these)

- One binary `axon`. `src/main.rs:120-125` initialises the bus and mounts its dashboard router
  (`crates/axon-bus/src/serve.rs`) on `127.0.0.1:7777`. Every POST needs a per-boot token and
  a matching Origin; Host is checked against DNS rebinding (`serve.rs:204-250`). The page
  carries the token in a meta tag, so any local account can read it (accepted risk SEC-7,
  `docs/BUS-PLAN.md` §7 Security).
- Bus model, `docs/BUS-PLAN.md` §2 Data model and §3 Routing: agents, edges
  (tree / link / grant), messages (body ≤ 400 chars, refs by `path:L-L@sha`), claims,
  budgets, and a hash-chained append-only `events` log. Delivery happens inside each
  harness's hook (short-lived sync processes, `hook.rs`, `doorbell.rs`); peer text is
  framed as untrusted (`msg.rs`, `roster.rs`).
- Agents run bus commands through a PreToolUse relay (`relay.rs`), bounded and outside the
  sandbox. The intro tells each agent who it can reach (`roster.rs`), the playbook is
  `guide.rs`.
- Repo identity: `snapshot.rs` (`repo_of`) groups by git common-dir, a local path.
- There is no settings page yet. One is planned (capture/retention, usage retention,
  hook status, budgets, DB size).

## The user's requirement (verbatim intent)

Two developers, each running Axon locally, work in tandem. The two Axons connect over a
secure P2P protocol with a real handshake. A peer registry lets agents reach the other
Axon's inbox. At registration, Axon enforces which project(s) may be messaged.
**Connecting, seeing connection status and health, and disconnecting at will are done
from the dashboard's settings page.**

## Proposed design (to be attacked)

1. **Identity.** Each Axon has a long-lived Ed25519 key (0600 in the data dir).
2. **Pairing from the settings page.** "Invite" shows a short one-time code (expires in
   10 min). The other user pastes it into "Join". A PAKE (SPAKE2 or CPace) over the code
   exchanges and pins both public keys; both screens show a short fingerprint to compare.
   After that, every connection is mutually authenticated against the pinned key.
3. **Transport.** Option A: iroh (QUIC, node id = Ed25519 key, hole punching, relay
   fallback that sees only ciphertext). Option B: mTLS/Noise over a network both machines
   already reach (LAN, Tailscale), no NAT traversal. Option C: a hosted relay. The proposal
   leans B first, A later.
4. **Peer registry.** Tables `peers (peer_id, pubkey, name, address, paired_at,
   revoked_at)` and `peer_projects (peer_id, project_key, direction)`. `project_key` is the
   normalised git remote URL (paths differ across machines). Pairing grants nothing; each
   project is shared explicitly, by both sides.
5. **Enforcement.** The receiver checks every inbound message: known key, not revoked,
   project granted, target agent's repo matches the project, size/rate caps. The sender
   checks too, so agents get a clear refusal. Everything is audited with the peer key.
6. **Agent surface.** Remote sessions in shared projects appear in `peers` and the intro as
   `<peer>:<agent-id>`. `send --to <peer>:<id>` forwards via the local Axon; replies come
   back on the same thread. Remote messages can never be `stop`, budget or claim
   operations, and never trigger the relay. `--ref` travels as metadata only.
7. **Settings page.** Peers list with state (connected / reconnecting / offline /
   revoked), last handshake, RTT, messages queued/sent/received, shared projects with
   toggles, a Disconnect (pause) and a Remove (revoke) action.
8. **Outbox.** Messages to an offline peer queue locally and retry; expire after 24 h.

## What to answer (your report has exactly these three sections)

### 1. Flaws
Anything in the proposal that is unsafe, wrong for this codebase, or overbuilt. Ranked,
each with the concrete failure and a fix. Pay particular attention to:
- Pairing initiated from a page whose token any local account can read (SEC-7). Does the
  network feature make SEC-7 unacceptable, and what is the minimal fix?
- Prompt injection from the remote human's agents into local agents; what the framing and
  the kind restrictions must be.
- Project identity: can a remote spoof a granted project by claiming a remote URL? Is
  remote URL the right key (forks, SSH vs HTTPS, no remote)?
- Metadata leakage: what does listing remote sessions reveal (cwd, mission, model)?
- Where the long-running network listener lives, given hooks are short-lived sync
  processes and the dashboard server is the only long-running process.
- Replay, downgrade, key compromise, revocation propagation.

### 2. Decisions
Your recommendation, with reasons, for: transport (A/B/C), PAKE vs other pairing, the
project key, whether "disconnect" and "remove" should be distinct, the minimum health
signals, and whether this should be a new crate or a module of `axon-bus`.

### 3. Missing acceptance criteria
Behaviours a test writer (a different vendor) can turn into acceptance tests before
implementation, especially failure paths (bad key, ungranted project, expired code,
replay, oversized message, peer offline, revoked mid-session). Be concrete: input, expected
observable result.
