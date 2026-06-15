# Contributing to Axon

Thanks for your interest! Axon is an open-source project conceived and maintained by
Daniel Tamas. Contributions are welcome — bug fixes, docs, and especially **new harness
parsers**.

By participating you agree to the [Code of Conduct](./CODE_OF_CONDUCT.md).

## Read this first

Axon is **spec-first**. The complete build spec is in **[DESIGN.md](./DESIGN.md)**, and its
§6 + Appendix A are **verified against real logs** — trust them over intuition. The committed
fixtures in [`tests/fixtures/`](./tests/fixtures/) are the acceptance gates: a change is
"done" when they pass and you've added a fixture for any new behavior.

## Dev setup

You need a stable Rust toolchain ([rustup](https://rustup.rs)). No Node is required to build —
`ui/dist` is committed and embedded via `rust-embed`.

```bash
cargo build                                  # debug build
cargo test                                   # run the M1 fixture gates + unit tests
cargo run -- --scan-only                     # scan your local Claude logs -> JSON

# Before opening a PR, these must be clean:
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## How to add a harness parser

This is the highest-value contribution. The pattern (see `src/ingest/claude.rs` as the
reference implementation):

1. **Verify against real logs first.** Open 2–3 real session files for the harness and
   confirm the actual shape — the timestamp format, the per-call/turn id, and whether one
   turn spans multiple lines. Do **not** assume from documentation (DESIGN.md §6.2/§6.3).
2. **Commit a redacted fixture** under `tests/fixtures/` capturing the shape + the numbers
   that matter, with the expected result written down (mirror `tests/fixtures/README.md`).
3. **Add the parser** as `src/ingest/<harness>.rs`, returning `Vec<RawTurn>`. Wire discovery
   into `src/ingest/mod.rs` and add the variant to `Harness` in `src/model.rs`.
4. **Map model ids** to canonical form in `canonicalize_model` (and add rates to
   `assets/pricing.toml` — leave unknown models to surface as `unpriced`, never a silent €0).
5. **Add gate tests** in `tests/` asserting collapse/attribution/cost against your fixture.

## Coding conventions

- Keep modules small and focused; match the surrounding style.
- English-only identifiers, comments, and logs.
- Validate at the boundary; never `unwrap()` on external/parsed input in library code — return
  `anyhow::Result` and let the caller decide.
- Cost must never be a silent €0: unknown models are `unpriced`, surfaced loudly.

## Commits & pull requests

- Use [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`,
  `docs:`, `refactor:`, `test:`, `chore:`).
- One logical change per PR; describe **what** and **why**, and note how you verified it.
- New behavior ships with a test. PRs must pass `fmt`, `clippy -D warnings`, and `test`.

## Reporting bugs

Open an issue with: what you ran, what you expected, what happened, and your OS + harness
versions. For parser bugs, a **redacted** sample log line is worth a thousand words.
