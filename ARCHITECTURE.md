# Axon architecture

How the shipped system is put together, as of 0.4.4. The original product spec is
[DESIGN.md](./DESIGN.md) (ingest schemas still hold, the frontend plan does not). The
control-plane contract is [docs/BUS-PLAN.md](./docs/BUS-PLAN.md), and federation's is
[docs/P2P-SPEC.md](./docs/P2P-SPEC.md). Dated changes and decisions go in [LOG.md](./LOG.md).

## One binary, one database, one page

```
 transcripts (~/.claude, ~/.codex, opencode, hermes) ──▶ axon-core ingest ──┐
 harness hooks ──stdin JSON──▶ axon bus hook <harness> <event> ─────────────┼──▶ SQLite (WAL)
 agents (via the hook relay) ──▶ axon bus send | claim | take | done | … ───┘    ~/.local/share/axon/axon.db
                                                                                  │
 axon (serve, 127.0.0.1:7777) ◀── reads the same file ── SSE /api/stream ──▶ dashboard
        └── federation service (iroh, opt-in) ◀── QUIC ──▶ paired machines
```

- **The database is the only state.** Every CLI and hook call is a short-lived process
  against one SQLite file. `serve` is a reader plus a few background loops. Killing it
  loses nothing.
- **Hooks never hurt the host session.** The budget is 300 ms. On error a hook allows,
  except the stop/budget gate, which fails closed.
- **No async runtime outside `serve`**, so `hook` stays near the process-spawn floor.

## Workspace

| Crate | Role |
|---|---|
| `axon` (root, `src/`) | The binary. `axon` scans, serves the dashboard and opens the browser. `axon bus <cmd>` hands off to `axon_bus::cli_main`. Also handles summary, export, config and the RTK stamp. |
| `crates/axon-core` | Ingest. Parses transcripts (Claude, Codex, OpenCode, ccflare) and normalizes them into events. Also owns pricing (`assets/pricing.toml`) and the usage store. |
| `crates/axon-bus` | The control plane. Includes the hooks, the registry, routing, messages, claims, the handled ledger, budgets, the dashboard server and UI, and federation. |

Releases are built by cargo-dist when a `v*` tag is pushed (macOS arm64 and x64, Linux musl,
Windows MSVC, plus a Homebrew formula).

## axon-bus modules

| Concern | Files |
|---|---|
| Schema, migrations, hash-chained `events` | `store.rs`, `store_migrate.rs` |
| Agents: register, status, orphan reap, memory | `registry.rs`, `memory.rs`, `session.rs`, `end.rs` |
| Hook entry: payload → verdict → harness reply | `hook.rs`, `codex_hooks.rs`, `hermes_hooks.rs`, `opencode_plugin.rs`, `gate.rs`, `guard*` |
| Agent commands run through the hook relay | `relay.rs` (relayed verbs), `cli_guard.rs` (injects `--agent`) |
| Who may message whom | `route.rs`: parent↔child edges, mutual `link`, thread grants, same-repo auto-link |
| Messages, doorbell, chatter | `msg.rs`, `doorbell.rs`, `chatter.rs` |
| File claims | `claims.rs` |
| Handled ledger (non-file work items) | `handled/mod.rs`, `handled/api.rs` |
| Budgets | `budget.rs` |
| SessionStart introduction and the guide | `roster.rs`, `guide.rs` |
| Install, doctor, uninstall | `install.rs`, `setup.rs`, `doctor.rs`, `uninstall.rs` |
| Dashboard server, snapshot, settings API | `serve.rs`, `snapshot.rs`, `observed.rs`, `transcript.rs`, `settings/` |
| Embedded UI | `ui/` (plain ES modules), listed in `assets.rs` |
| Federation | `fed/` (below) |

### Who an agent can reach (`route.rs`)

- A parent and its child can always message each other.
- Two roots can message each other through a mutual `link` edge, or through **same-repo
  auto-link**. Two live roots (active or idle, not subagents, of a known harness) whose cwd
  resolves through `repo_of` to the same repository are connected without operator action.
  It is on by default, and Settings → Agents turns it off (`settings.auto_link = "0"`).
- Thread grants allow a reply within one thread.

### Handled ledger (`handled/`)

The ledger is one per repository, keyed by `repo_of(cwd)`, so worktrees share their main
repository's ledger. A row is `(repo, key)` → `taken | done | free`, plus holder, note, `at`,
`expires_at` and `changed_at`.

- **Commands:** `take`, `done`, `drop` and `handled` are relayed agent verbs. They exit 0, or
  exit 1 with the line naming the holder.
- **A take ends** when it expires (2h by default) or its taker closes.
- **Retention:** done rows are kept 90 days and free rows 7 days. Each command and `serve`
  (every 10 s) apply it.
- **Dashboard:** `GET /api/handled?repo=` feeds the project view's "Handled work" panel.
- **The table's DDL lives in `store.rs`** (`HANDLED_SCHEMA`), because a frozen test compiles
  `store.rs` standalone.

## Federation (`fed/`)

Federation is opt-in (Settings or the Connections view). It runs iroh QUIC with pinned node
ids and one protocol, `axon/fed/1`, carrying request/response JSON frames. Each frame kind
is registered with `Handle::on_frame` and rate-limited per peer.

| Concern | Files |
|---|---|
| Service, transport, framing, rate limits | `service.rs`, `transport.rs`, `codec.rs`, `rate.rs`, `lock.rs` |
| Identity, invites, pairing | `identity.rs`, `invite.rs`, `pairing*` |
| Shares: a repository offered and accepted per peer, with in/out flags | `shares*` |
| Discovery: the peer's sessions per share, re-read every 30 s | `discovery*` |
| Messages: outbox, delivery, receive, envelopes | `outbox*`, `delivery*`, `receive*`, `envelope.rs` |
| Handled ledger sync (P2P-SPEC §7b) | `handled_sync.rs` |
| Peer lifecycle, reconcile, retention, health, stats | `lifecycle*`, `reconcile*`, `retention*`, `health.rs`, `stats.rs` |
| Owner HTTP API | `api*` |

**Ledger sync.** Every 2 s, for each share that is active and has outbound set here and
inbound set there, local entries changed since the peer's last ack are sent as a `handled`
frame, at most 200 entries per frame.

- **Cursor:** kept in memory per (share, generation). A failure or restart triggers a full
  send of the live entries.
- **Holder label:** the holder's discovery label (`<harness>-<4 chars>`). The session is
  minted at send time, so the label never changes after it is first sent.
- **Receiving:** entries are filed with `peer_id` set and are never echoed back.
- **Conflicts:** done beats taken. Otherwise the earlier `at` wins, and a tie goes to the
  lower node id.
- **Older peers:** a peer that answers `unknown_frame` is skipped for 10 minutes.
- **Ending:** unsharing or unpairing deletes that peer's entries.

## Dashboard (`crates/axon-bus/ui`)

The dashboard is vanilla ES modules. The CSP allows self only, so there are no inline
styles. It has light and dark tokens and respects reduced motion. The hash is the route:

| Route | View |
|---|---|
| `#/` | Projects |
| `#/p/<repo>` | One project: topology, handled work, and the context rail |
| `#/usage` | Usage |
| `#/connections` | Link map and peers |
| `#/settings` | Settings |

Live data arrives over `/api/stream` (`snapshot` and `fed` events), with polls as the
fallback. Every file served must appear in `src/assets.rs`.

## Testing

Acceptance tests (`crates/axon-bus/tests/acceptance_*.rs`) are written from the spec by a
different vendor (Codex) and frozen. The implementer never edits them, and a red test is
fixed in `src`. They run against real loopback nodes, real HTTP and isolated
`HOME`/`XDG_*` directories, and never touch the user's real harness data.
