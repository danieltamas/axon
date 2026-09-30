# Release readiness: v0.3.0 (branch `job/axon-bus/m0`)

Checked 2026-10-01 against 26 commits ahead of `main` (121 files, +16.2K / −216). The branch
has never been pushed.

## Verdict

**Not ready to tag.** It is ready to push as a PR once three mechanical items are fixed (fmt,
the one red test, the stale `bin/` install path). Before tagging, also: the privacy findings
(R1–R5) re-verified, a version bump, and README/DESIGN brought up to date.

## Evidence (run locally, same commands as `.github/workflows/ci.yml`)

| CI step | Result |
|---|---|
| `cargo fmt --all --check` | **fails**: 9 files (8 in `crates/axon-bus/src`, `src/server.rs`), pre-existing drift |
| `cargo clippy --workspace --all-targets -- -D warnings` | passes |
| `cargo test --workspace --all-targets` (macOS arm64) | **1 failure**: `test_4_usd_only_budget_denies_unpriced_captured_model` (frozen acceptance test; the second previously known failure now passes) |
| Windows / Linux / macOS-13 test matrix | **not verified**; only `aarch64-apple-darwin` and `x86_64-unknown-linux-gnu` targets are installed here |

## Blockers, in order

1. **fmt drift.** Run `cargo fmt --all` as its own `style:` commit. CI fails on it otherwise.
2. **Red acceptance test.** `test_4_usd_only_budget_denies_unpriced_captured_model`: an
   unpriced captured model must be denied under a USD-only budget. Fix it in `src`; the test is
   frozen.
3. **`bin/` + README install path.** The README installs from `raw/main/bin/axon-*`: 17 MB of
   0.2.1-era binaries committed to git. After the merge, `curl` users get the old binary while
   the README describes the new app. Recommendation: delete `bin/` and point Install at the
   GitHub Release installers that dist already generates (shell, PowerShell, Homebrew).
4. **Privacy review status.** `docs/reviews/codex-bus-observed-2026-09-30.md`: R1–R5 (P1:
   capture-flag bypass, cross-server lease leak, gate fail-open on a DB lock, message bodies
   bypassing redaction, redactor misses) were addressed in `90f9d55`. They need an independent
   re-check before a public release, because content capture is now **on by default**.
5. **R6 is now user-facing.** R6 (P2): two processes in the same directory can swap
   transcripts. With per-card **End**, a swapped mission line can lead someone to end the wrong
   session. The signal itself targets the pid shown, so the risk is mislabelling, not
   mistargeting. Fix it, or show only pid and model on cards where the match is ambiguous.
6. **Cross-vendor review** of the commits after `90f9d55` (hook wiring, `/api/end` process
   signalling, incremental scan). None of them has been reviewed by a second vendor.

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
- **Windows:** hook commands are written with POSIX single-quote escaping, and `/api/end`
  sends SIGTERM (sysinfo returns false where unsupported). The Windows CI job is the first
  real test of both.
- **MSRV:** `rust-version = "1.74"` has not been checked against the new dependency set
  (sysinfo 0.33, toml_edit 0.22, axum 0.7). Add a `cargo +1.74 check` job, or raise the
  declared MSRV to what the lockfile needs.
- **crates.io:** not part of the flow. If it becomes one, `axon-bus` must be published first
  and the root's `axon-bus = { path = … }` dependency needs a `version`.
- **Repo hygiene:**
  - `graphify-out/` is untracked; add it to `.gitignore`.
  - `.playwright-mcp/` is already ignored.
  - `ui/dist/index.html` (the old Mission Control page) is still tracked, but nothing embeds it
    any more (no `RustEmbed` or `ui/dist` reference in `src/` or `crates/`), and `rust-embed`
    is an unused dependency. Remove both, along with the `ui/dist` lines in `.gitignore`.

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
app (PWA manifest and icons).

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
