# Axon UI — "Mission Control"

The browser dashboard. **It is a single self-contained file: [`dist/index.html`](./dist/index.html).**

No framework, no bundler, no Node, no build step. Vanilla HTML + CSS + a single `<script>`
(canvas 2D for the neural mesh + wave, plain DOM for everything else). That one file **is**
the source — edit it directly.

## How it ships

`dist/index.html` is **committed** and compiled straight into the Rust binary:

```rust
// src/server.rs
const INDEX_HTML: &str = include_str!("../ui/dist/index.html");
```

So `cargo build` / `cargo install axon` needs no Node and produces a binary that serves the
dashboard at `GET /`. Editing the file and rebuilding is the entire UI workflow.

## Develop

```sh
# 1. run a backend in one terminal (writes to SQLite, serves the API on loopback)
cargo run                 # serves http://127.0.0.1:<port> — see startup log for the port

# 2. edit ui/dist/index.html in your editor

# 3. see your change
cargo run                 # rebuild embeds the new file, then hard-refresh the browser
```

Want a faster edit loop without rebuilding the binary? Open `ui/dist/index.html` directly in a
browser and point its one `fetch` at a running backend (Origin is loopback-only, so serve it
from `127.0.0.1`). Either way, the source of truth you commit is `dist/index.html`.

## Data contract

The dashboard is a thin client over **one** endpoint — SQLite is the source of truth:

| Call | Returns |
| --- | --- |
| `GET /api/summary?range=today\|7d\|30d\|all` | the whole dashboard payload: KPIs, per-agent / per-model rows, cost-by-harness, RTK savings, budget, and `recent` (newest turns for the live feed) |
| `GET /api/health` | liveness |

The UI **polls** `/api/summary` and re-renders; there is no SSE/WebSocket in the current build.
All numbers come from the API — never accumulate counters client-side. The range buttons
(`TODAY / 7D / 30D / ALL`) just change the `range=` query param.

## Non-negotiable constraints (DESIGN.md §16)

- **Zero outbound. No CDN.** Bundle/inline every asset. No `http(s)://` asset literal may
  appear in `dist/` — fonts, scripts, styles, images all stay local.
- **Fonts:** use locally installed JetBrains Mono / Space Grotesk *if present*, else fall back
  to system mono/sans. Never fetch a remote font.
- **Loopback only.** The page is served to `127.0.0.1` and talks only to the local backend.
  Nothing leaves the machine.

When you touch the UI, grep your diff for `http://` / `https://` asset references and remote
`@import` / `<link>` / `<script src>` before committing — that's what keeps the "zero egress"
promise true.

## Roadmap

DESIGN.md §10.1 / §14 sketch a richer 3D "live brain" (originally floated as a Vue 3 + TresJS
rewrite). That has **not** been built and is not the current contribution surface — today's
dashboard is the single vanilla file above. If/when a heavier stack lands, this README and the
embed wiring will change together; until then, contribute to `dist/index.html`.
