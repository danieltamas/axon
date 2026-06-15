# Axon

**Local, harness-agnostic observability for AI coding agents — one Rust binary, in your browser.**

Axon reads the logs your AI coding-agent harnesses already write (Claude Code, Codex, OpenCode) and shows — live — *which harness, project, agent, and model* is working, how fast, at what cost, and how much code it's producing. The hero view is a three.js "brain" of models firing in real time; alongside it: per-agent/model cost & attribution, local leaderboards, budget alerts, and shareable cards.

**100% local. No server. No account. Nothing leaves your machine.**

> **Status: spec stage — not yet implemented.** The complete, verified build spec is in **[DESIGN.md](./DESIGN.md)**. Start at milestone **M1** (§14).

## Why it exists
`ccusage` is Claude-Code-only, CLI, no per-agent, no visuals. `Langfuse`/`Grafana` are heavy multi-service self-hosts. Axon is **one binary, cross-harness, with exact per-named-agent attribution and a live visual** — the gap nobody fills.

## How it works
`axon` → parses `~/.claude`, `~/.codex`, OpenCode logs → SQLite → serves an embedded **Vue 3 + TresJS** dashboard at `http://localhost:7777` and opens your browser. Live updates via file-watch → SSE. `ui/dist` is committed and embedded via `rust-embed`, so `cargo install axon` needs no Node.

## Build it
Read **DESIGN.md** end to end — the data schemas in **§6 + Appendix A are verified against real logs**, trust them over intuition. Then:
1. **M1** — Claude Code ingest (incl. sub-agent attribution) → SQLite → `--scan-only`. Gate: the fixtures in [`tests/fixtures/`](./tests/fixtures/) parse to the expected Events (see `tests/fixtures/README.md`).
2. **M2** — Codex + OpenCode (verify their real logs first), cost, model map.
3. … see DESIGN.md §14.

## Privacy
Loopback-only bind + Origin checks, all UI assets bundled (enforced zero-outbound), SQLite `0600`. See DESIGN.md §16.

## License
MIT (proposed — add a `LICENSE` file).
