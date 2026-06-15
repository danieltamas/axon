# Axon UI (Vue 3 + TresJS)

The dashboard. Build output (`ui/dist`) is **committed** and embedded into the Rust binary via `rust-embed`, so `cargo build` / `cargo install axon` needs no Node.

- **Stack:** Vue 3 + Vite + TresJS (three.js) + `@tresjs/cientos` + postprocessing (bloom) + Pinia + `vue-echarts` + native `EventSource`.
- **Landing = "Mission Control"** (DESIGN.md §10.1): the live brain (nodes = models, size = **cost**, color = harness, **stable radial layout**) **plus** a cost/budget HUD on the same screen.
- **Numbers come from `/api/*`** (SQLite is the source of truth). SSE (`/events`) is **decorative only** — pulses may drop; never accumulate counters from it.
- **Zero outbound (enforced):** bundle every asset — fonts, three.js, ECharts, HDRIs. **No CDN.** A build-time check must fail on any `http(s)://` literal in `dist/` (DESIGN.md §16).

## Dev
```
cd ui
npm create vite@latest .      # Vue + TS
# add: three @tresjs/core @tresjs/cientos pinia echarts vue-echarts
npm run dev                   # against a running `axon` backend
npm run build                 # -> dist/  (commit it)
```
