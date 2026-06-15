# Axon

**Local, harness-agnostic observability for AI coding agents — one Rust binary, in your browser.**

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](./LICENSE)
[![Status: early access](https://img.shields.io/badge/status-early%20access-orange.svg)](#roadmap)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg?logo=rust)](https://www.rust-lang.org)

Axon reads the logs your AI coding-agent harnesses **already write** — Claude Code, Codex,
OpenCode — and shows *which harness, project, agent, and model* is working, how fast, at what
cost, and how much code it's producing. The signature view is a live three.js "brain" of your
models firing in real time; alongside it sit per-agent/model cost & attribution, local
leaderboards, budget alerts, and shareable cards.

**100% local. No server. No account. Nothing leaves your machine.**

> [!NOTE]
> **Early access.** The first working slice ships today: **Claude Code ingestion** with exact
> per-sub-agent attribution, available via `axon --scan-only` (JSON). The live web dashboard
> and the 3D brain are in active development — see the [roadmap](#roadmap). The complete,
> fixture-verified build spec lives in **[DESIGN.md](./DESIGN.md)**.

## What it does

| | Capability | Status |
|---|---|---|
| 🧩 | **Cross-harness** ingest from the logs harnesses already write (Claude Code, Codex, OpenCode) | Claude ✅ · others 🔭 |
| 🎯 | **Exact per-named-agent attribution** — see what each sub-agent (`coder`, `security`, `Explore`…) actually cost, no heuristics | ✅ |
| 💸 | **Cost & budget awareness** — per model/agent/project, with daily/weekly budget alerts | engine ✅ · alerts 🔭 |
| 🧠 | **Live "brain"** — three.js view of models firing, sized by cost | 🔭 |
| 🏆 | **Local leaderboards + Hall of Fame** and shareable PNG/WebM cards | 🔭 |
| 🔒 | **Enforced privacy** — loopback-only, assets bundled, SQLite `0600`, zero egress by default | ✅ (design) |

Legend: ✅ available · 🚧 in progress · 🔭 planned.

## Why Axon

| | Axon | `ccusage` | Langfuse / Grafana+OTEL |
|---|---|---|---|
| Scope | **Cross-harness** | Claude Code only | App-level / Claude-only |
| Per-named-agent attribution | **✅ exact** | ✗ | ✗ |
| Local, single binary | **✅** | ✅ (npx) | ✗ (Docker + Postgres/Clickhouse) |
| Live visual + budget alerts | **✅ (planned)** | ✗ | partial |
| Account / server required | **No** | No | Yes |

`ccusage` is the instant incumbent for Claude-only token tables. Axon's wedge is
**cross-harness cost + exact per-agent attribution in one local pane**, with a live visual on
top — the gap nobody fills.

## Install

> Pre-built binaries, the `curl` installer, and the Homebrew tap land with the **first tagged
> release**. Until then, build from source.

```bash
# From source (needs a Rust toolchain — https://rustup.rs)
git clone https://github.com/danieltamas/axon
cd axon
cargo build --release        # -> target/release/axon

# Planned for the first release:
# cargo install axon
# brew install danieltamas/tap/axon
# curl -fsSL https://github.com/danieltamas/axon/releases/latest/download/install.sh | sh
```

## Usage

```bash
# Available now — scan your local Claude Code logs and print a JSON summary, then exit:
axon --scan-only

# Planned — start the local dashboard and open the browser:
axon                 # serves http://localhost:7777
axon --port 8080 --no-open
```

`axon --scan-only` walks `~/.claude/projects/`, collapses each turn, attributes every
sub-agent, and prints totals plus per-model and per-agent breakdowns:

```jsonc
{
  "events": 35929,
  "sessions": 97,
  "tokens_out": 29126648,
  "cost_eur": 0.0,                       // 0.0 until you set real rates in pricing.toml
  "unpriced_models": ["claude-opus-4.7"],// models missing from the map, surfaced loudly
  "unattributed_token_pct": 0.0,         // honesty gauge: tokens not tied to a named agent
  "by_agent": [
    { "agent": "main",       "tokens_out": 26875175 },
    { "agent": "general-purpose", "tokens_out": 1813567 },
    { "agent": "security",   "tokens_out": 49300 }
  ]
}
```

Cost reads `0.0` until you fill real rates in
[`assets/pricing.toml`](./assets/pricing.toml) (or `~/.config/axon/pricing.toml`) — the
placeholders are intentionally zero so they can never masquerade as truth.

## How it works

`axon` parses `~/.claude` (and, soon, `~/.codex` / OpenCode) logs into a normalized event,
stores them in an embedded SQLite database, and serves an embedded **Vue 3 + TresJS**
dashboard. Live updates come from file-watch → SSE; the numbers always come from SQLite (the
source of truth). `ui/dist` is committed and embedded via `rust-embed`, so `cargo install
axon` needs no Node toolchain. See **[DESIGN.md](./DESIGN.md)** for the verified data schemas
(§6 + Appendix A are checked against real logs — trust them over intuition).

## Privacy

Loopback-only bind (`127.0.0.1`/`[::1]`) + Origin checks; every UI asset bundled (enforced
zero-outbound — no CDN); SQLite stored `0600` in a `0700` directory. Share-cards redact
project/branch by default. Nothing leaves your machine unless you export a file or opt into
`--otel`. See [DESIGN.md §16](./DESIGN.md).

## Roadmap

- **M1 — Claude ingest + store (headless)** ✅ *collapse-by-`message.id`, exact sub-agent attribution, cost/LOC engine, SQLite, `--scan-only`.*
- **M2 — Codex + OpenCode** 🔭 *verify real logs, canonical model map, real € rates.*
- **M3 — Server + analytics UI** 🔭 *axum + `/api/*` + embedded Vue; leaderboards, drill-down, budget alerts, CSV/JSON export.*
- **M4 — Brain + live** 🔭 *file-watch → SSE, Mission-Control landing, 60fps perf gates.*
- **M5 — Cards/clips + packaging** 🔭 *PNG/WebM, weekly recap, cross-compiled releases, installer, Homebrew.*
- **M6 — OTEL export (optional)** 🔭

Full detail in [DESIGN.md §14](./DESIGN.md).

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
