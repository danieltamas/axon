# Release readiness: v0.3.0 (branch `job/axon-bus/m0`)

Checked 2026-10-01 against 26 commits ahead of `main` (121 files, +16.2K / −216). The branch
has never been pushed.

## Status

All six blockers below are closed on the branch. What remains is the push sequence: PR, the CI
matrix (Windows is the first real run), merge, version bump and tag.

| # | Blocker | Resolution |
|---|---|---|
| 1 | fmt drift | `2014723 style: cargo fmt` |
| 2 | Red acceptance test | `61741e4`, `012aa8c`: the test now uses a synthetic unpriced model id; green |
| 3 | `bin/` + README install path | `b8d872a`: `bin/` removed, README installs from GitHub Releases |
| 4 | R1–R5 re-check | Codex re-review: all five FIXED (R5 partial until #10 below) |
| 5 | R6 transcript swap | `cfc4d43`, `00f48c3`: matched on evidence; interleaved sessions stay unattributed |
| 6 | Cross-vendor review of the later commits | Codex review of `90f9d55..61f7916`; findings closed below |

### Second Codex review (`61f7916`), findings

| # | Finding | Status |
|---|---|---|
| 1 | P1 pid reuse can end another process | FIXED `73fc617`: requests carry `{id, started_ms}`; the pid is re-read and refused when its start time differs. Residual: the microseconds between that re-read and the signal (macOS has no pidfd) |
| 2 | P1 basename is weak session identity | **Accepted risk**: only a caller with the per-boot token can ask, and anyone who can run a binary named `codex` already controls the account |
| 3 | P1 R6 swaps resumed sessions | FIXED `00f48c3` |
| 4 | P1 Hermes install corrupts valid YAML | FIXED `dc357b2`: commented and indentless `hooks:` blocks merge; `hooks: {}` is refused |
| 5 | P2 Windows hook commands omit `bus` | FIXED `deab640`: `.exe` and backslash paths keep the verb; cmd double quotes |
| 6 | P2 failed reads cached as success | FIXED `5e82225`: unreadable sources stay unstamped |
| 7 | P2 cache identity misses changes | FIXED `5e82225`, `19db64a`: parser version and bundled prices key the cache; each file (transcript, `.meta.json`, WAL) contributes its own size, mtime and change marker (inode + ctime on Unix, a head/tail hash on Windows) |
| 8 | P2 truncated Codex history double-counts | **Deferred**: pre-existing in 0.2.1, needs baseline reconciliation in ingest |
| 9 | P2 wiring check accepts missing hooks | FIXED `43f383b`: every required event, and OpenCode registration |
| 10 | P2 numeric access tokens disclosed | FIXED `8bb00f4`: only `*tokens` / `token_count` count fields stay readable |

Codex wrote 36 acceptance tests for these fixes (`1b99fe2`); 8 failed and were fixed in src
(`19db64a`): column-0 comments inside Hermes `hooks:`, commented-out Hermes hooks counted as
wired, `password_tokens=` style secrets kept readable, `=` in an executable path bypassing the
guard, and the service worker deleting other apps' caches on the same origin. All green; the two
Windows-only tests run in CI. Report: `docs/audits/axon-bus-review2-2026-10-01.md`.

### Known limitations (release notes)

- **Two versions, one database.** Each binary keys the scan cache on its own version, so a
  0.2.x and a 0.3.0 `axon` running at the same time clear each other's stamps and every scan
  becomes a full re-read (15.9 s observed against 0.7 s). Stop the old one after upgrading.
- **Two servers starting at once** can hit `database is locked` on startup; the second start
  succeeds on retry.
- **CLI guard on Windows bash** splits commands on `/` and `\`, so an argument that merely
  contains a backslash path ending in `axon` is still parsed as an invocation.

## Release mechanics

- **Version:** bump the workspace to **0.3.0**. The DB migration (`events` → `usage_events`,
  plus the bus tables, `scanned_sources` and `scan_meta`) is one-way: running 0.2.1 against a
  0.3.0 database collides on `events`. Say so in the release notes. The bump also changes the
  scan fingerprint, so every install re-prices its history once on first start (~17 s here, in
  the background).
- **Tag flow:** `.github/workflows/release.yml` (dist 0.32.0) runs on a pushed `v*` tag. It
  builds five targets (macOS arm64/x64, Linux musl arm64/x64, Windows x64) plus shell,
  PowerShell and Homebrew installers, and publishes to `danieltamas/homebrew-tap`.
  - The tap repo exists.
  - `gh secret list` showed no `HOMEBREW_TAP_TOKEN` (or no permission to see it). Confirm it
    exists or the Homebrew publish job fails.
- **What ships:** only the `axon` binary. `axon-bus` is `dist = false`, and hooks are wired as
  `<path>/axon bus hook …`, so the alias is not needed.
- **Hook paths after upgrades:** hooks store the absolute path of the `axon` that wired them,
  and every `axon` start re-wires if the path changed.
  - Risk: a Homebrew upgrade on Linux (where `current_exe` resolves to the versioned Cellar
    path) leaves hooks pointing at a deleted binary until the next `axon` start.
  - Claude Code treats a missing hook command as non-blocking. Not verified for Codex, OpenCode
    or Hermes.
- **Windows:** hook commands use forward slashes and cmd double quotes; `/api/end` relies on
  sysinfo's terminate, which returns false where unsupported. The Windows CI job is the first
  real test of both.
- **MSRV:** `rust-version = "1.86"`, checked by the `msrv` CI job (`61f7916`).
- **crates.io:** not part of the flow. If it becomes one, `axon-bus` must be published first
  and the root's `axon-bus = { path = … }` dependency needs a `version`.
- **Repo hygiene:** done. `graphify-out/` is ignored; `ui/dist` and `rust-embed` are gone.

## Push sequence

1. `style: cargo fmt` commit; fix the red test in `src`; remove `bin/` and rewrite README
   Install / Usage / Privacy / Roadmap.
2. Push `job/axon-bus/m0` and open a PR to `main`, which runs the full CI matrix (macOS ×2,
   Linux ×2, Windows).
3. Fix what the matrix finds (Windows is the likely one); cross-vendor review and R6 run in
   parallel.
4. Squash or merge to `main`, bump to 0.3.0 (`chore(release): v0.3.0`), write the release notes
   below, tag `v0.3.0`, push the tag.
5. Check the release page artifacts, `curl | sh` on a clean machine, `brew install
   danieltamas/tap/axon`, and a first run that wires hooks.

## New capabilities since v0.2.1

**One app.** `axon` serves a single dashboard on `127.0.0.1:7777`. It combines the usage
analytics, the live brain, and the new control plane (`axon bus …`). It installs as a desktop
app (PWA manifest and icons). In a browser, a banner offers installation (Chromium's install
prompt, or the menu path on Safari). When `axon` is not running, the installed app shows an
offline page with the command to start it, and returns to the dashboard by itself once it
answers.

**Zero-setup hooks.** The first run wires Claude Code, Codex, OpenCode and Hermes:
- config backed up first;
- Hermes hooks merged into the owner's existing `hooks:` block;
- development builds refused;
- `axon bus uninstall` restores byte-exact;
- `--no-hooks` opts out.

**Live topology.**
- Projects overview:
  - figures;
  - "Working now" and "Quiet" groups;
  - what each project needs from you.
- Project page:
  - sessions grouped as Needs you, Working, Idle, Closed, newest first;
  - subagents beside their session;
  - a 24 h activity chart;
  - tokens, spend and memory per session.

**Observed sessions.**
- Harness processes started before the hooks are discovered from the process table.
- Each one shows its transcript narrative ("Now / Said / Ran"), model, tokens, cost and
  resident memory.

**Ending sessions.** End any open session from its card, from its detail pane, or all idle ones
in a project at once. Each needs a two-click confirm. The server re-checks every id and sends
the terminate signal only to a process it found in its latest scan.

**Messaging between agents** (`send`, `ask`, `reply`).
- Routed along edges;
- root-to-root `link` / `accept`;
- temporary `grant`s per thread;
- operator messages from the dashboard composer;
- threads with owed-reply tracking.

**Budgets and stops.**
- Token and USD ceilings per tree or per agent, with warn at 80% and a stop gate;
- stop doorbells;
- priced via Claude usage ingest;
- stale-usage stop threshold.

**Coordination.**
- `claim` / `release` / `claims` for paths in a checkout;
- task routing from `routes.toml` (`route`, `spawn` for virtual agents).

**Integrity and replay.** A hash-chained audit log (`audit`) and `replay` of a JSONL transcript
corpus.

**Privacy controls.**
- Content capture on by default, with `--no-content` for structure only;
- credential redaction in the captured narrative;
- a capture lease shared between servers;
- a CLI guard that stops agents from starting capture-on servers;
- a token and Origin guard on every write API.

**Pricing.** Rates for the Claude 5 family and gpt-6-astra; display currency on every figure.

**Fast start.** The dashboard answers in about 0.7 s. Only changed logs are re-read (warm scan
0.6 s, was 17.6 s over 4.5 GB), and the same applies to the live refresh while agents work.

**Workspace.** `axon-core` (ingest, pricing, store), `axon-bus` (control plane, dashboard),
`axon` (the app). Releases are built with cargo-dist.
