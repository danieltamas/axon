<p align="center">
  <img src="assets/socials/axon-github-banner-1280x640.png" alt="Axon: local observability and control for AI coding agents" width="100%">
</p>

# Axon

**See every AI coding agent on your machine, what it costs, and steer it, from one local dashboard.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Status: early access](https://img.shields.io/badge/status-early%20access-orange.svg)](#roadmap)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg?logo=rust)](https://www.rust-lang.org)

Axon reads the logs your coding-agent harnesses already write (Claude Code, Codex, OpenCode,
Hermes) and wires itself into their hooks. One `axon` binary then shows which sessions and
subagents are running in which project, what each one is doing and costs, and lets you message,
budget, stop or end them.

**100% local. No account. Nothing leaves your machine.**

## What it does

| Capability | |
|---|---|
| **Live topology**: every project, its sessions grouped as Needs you / Working / Idle / Closed, subagents beside their session, a 24 h activity chart | ✅ |
| **Observed sessions**: harness processes started before Axon are found from the process table, with transcript narrative ("Now / Said / Ran"), model, tokens, cost and memory | ✅ |
| **End sessions**: end one idle session from its card or every idle one in a project; two-click confirm, re-checked by the server | ✅ |
| **Messages between agents**: `send` / `ask` / `reply` along edges, root-to-root `link`, per-thread `grant`, operator messages from the dashboard | ✅ |
| **Budgets and stops**: token and USD ceilings per tree or agent; warn at 80 %, a stop gate at 100 % | ✅ |
| **Cross-harness cost**: exact per-subagent attribution, per model / agent / harness, today / week / month spend with budget alerts | ✅ |
| **Coordination**: path `claim`s in a checkout, task routing from `routes.toml` | ✅ |
| **Audit and replay**: hash-chained audit log, replay of a JSONL transcript corpus | ✅ |
| **Usage brain**: a live view of models firing, sized by spend | ✅ |
| **RTK**: Rust Token Killer's token savings, if installed | ✅ |
| **Desktop app**: install the dashboard as an app from the browser; when `axon` is not running it shows how to start it and comes back by itself | ✅ |
| Shareable cards, OTEL export | planned |

## Install

```bash
# macOS and Linux
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/danieltamas/axon/releases/latest/download/axon-installer.sh | sh

# Homebrew
brew install danieltamas/tap/axon
```

```powershell
# Windows
powershell -ExecutionPolicy Bypass -c "irm https://github.com/danieltamas/axon/releases/latest/download/axon-installer.ps1 | iex"
```

Each [release](https://github.com/danieltamas/axon/releases) also carries plain archives and
checksums for macOS (Apple Silicon, Intel), Linux (x86_64, arm64; static musl) and Windows.
From source, with [Rust](https://rustup.rs): `cargo install --path .` in a clone.

The first run wires Axon into the harnesses it finds: hooks in their own configs that run this
`axon`, which is what makes messages, budgets and stops work. Each config is backed up first;
`axon bus uninstall` restores it byte for byte, and `axon --no-hooks` leaves them alone. If you
move or reinstall `axon`, the hooks are repointed on its next run.

> [!NOTE]
> **Upgrading from 0.2.x** moves Axon's usage rows to a new table and re-reads your logs once on
> the first start (in the background; the dashboard is up immediately). The migration is
> one-way: don't run 0.2.x against the database afterwards.

## Usage

```bash
axon                      # serves http://127.0.0.1:7777 and opens it
axon --port 8080 --no-open
axon --no-content         # structure only: turns, tokens and tool names, never text
axon --scan-only          # headless: scan the logs, print a JSON summary, exit

axon bus --help           # the control plane: send, ask, budget, claim, audit, doctor…
axon bus budget set <root> 2Mtok --usd 20
axon bus doctor           # which harnesses are wired
```

Axon scans `~/.claude/projects/`, `~/.codex/sessions/`, OpenCode's `opencode.db` and
ccflare-family proxy databases. Only logs that changed since the last scan are read again, so
a restart and the live refresh stay fast however much history you have. Budget caps live in
`~/.config/axon/config.toml` (see [`assets/config.example.toml`](./assets/config.example.toml)).

`axon --scan-only` prints totals plus per-model, per-agent and per-harness breakdowns:

```jsonc
{
  "events": 38483,
  "sessions": 206,
  "tokens_out": 36320161,
  "cost_eur": 8187.32,                    // computed from pricing.toml (USD rates × fx)
  "unpriced_models": ["gpt-5.4"],        // models missing from the map: cost is a floor
  "today_cost_eur": 409.96,
  "by_harness": [
    { "harness": "claude-code", "cost_eur": 6963.44 },
    { "harness": "codex",       "cost_eur": 1215.15 },
    { "harness": "opencode",    "cost_eur": 8.73 }
  ]
}
```

Rates are USD from each provider's docs, converted via `fx_to_display` in
[`crates/axon-core/assets/pricing.toml`](./crates/axon-core/assets/pricing.toml) (override at
`~/.config/axon/pricing.toml`). A model missing from the map is flagged **unpriced** rather than
counted as free; local models are free; OpenCode's own per-message cost is used directly.

## How it works

Three crates in one workspace:
- `axon-core`: harness log ingest, normalisation, pricing and the SQLite store.
- `axon-bus`: the control plane, with registry, hooks, routed messages, budgets, claims and
  the audit log, plus the dashboard it serves.
- `axon`: the app that ties them together.

Harness hooks call `axon bus hook …` on each event. The dashboard is plain ES modules embedded
in the binary, live over SSE, and SQLite is the source of truth. See
[DESIGN.md](./DESIGN.md) for the ingest schemas and [docs/BUS-PLAN.md](./docs/BUS-PLAN.md) for
the control plane.

## Privacy

- The server binds loopback only (`127.0.0.1`).
- Every write API checks the Host and Origin headers and needs a per-boot token.
- All UI assets are bundled, so nothing loads from a CDN.
- SQLite is stored `0600` in a `0700` directory.
- Agents' conversation text is captured by default for the narrative view, with credentials
  redacted. Run `axon --no-content` for structure only.
- Agents cannot start a capture-on server themselves.
- Nothing leaves your machine unless you export a file or opt into `--otel`.

## Roadmap

- **Done:** cross-harness ingest (Claude Code, Codex, OpenCode, ccflare); live dashboard and
  usage brain; the control plane (registry, hooks, messages, budgets, claims, audit, replay);
  observed sessions and ending them; incremental scanning; releases with installers.
- **Next:** shareable cards and weekly recap; spawning real agents from `routes.toml`; OTEL
  export.

## Contributing

Contributions are welcome — especially **new harness parsers**. Start with
[CONTRIBUTING.md](./CONTRIBUTING.md) and read [DESIGN.md](./DESIGN.md); the fixtures in
[`tests/fixtures/`](./tests/fixtures/) are the acceptance gates. By participating you agree to
the [Code of Conduct](./CODE_OF_CONDUCT.md).

## License

[MIT](./LICENSE) © 2026 Daniel Tamas.

## Author

Conceived and created by **Daniel Tamas**. If Axon is useful to you, a ⭐ on the
[repo](https://github.com/danieltamas/axon) is the best way to say thanks.
