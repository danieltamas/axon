# Axon — build spec **v1** (hand-off document)

> **Working name: Axon** (was "Cortex" — renamed: `cortex` collides head-on with the Jan/Menlo `cortex` LLM runtime — same binary, same space. `Axon` keeps the neuron metaphor with a freer namespace. Still changeable; alts: `Pulse`, `Synapse`.)

> A single, self-contained **Rust binary** that turns the logs your AI coding-agents already write into a **live, harness-agnostic observability dashboard** in the browser — live activity, spend, speed, and *which agent/model did what*, with a three.js "brain" as the signature view and shareable cards. **100% local, zero infra, one binary.**

> **This is a hand-off spec.** Copy it into the new `axon/` repo as `DESIGN.md` and execute. It assumes no prior context. **§6 + Appendix A are verified against real logs on the author's machine** — trust them over any prior recon.

---

## 0. What changed since the first draft (read this first)
A 3-reviewer council + a real-data fixture pass **invalidated three load-bearing claims** in the original draft. All are now fixed in this version:
- **Timestamps are ISO-8601 strings, not epoch-ms.** (70,014/70,014 real lines.) Parse RFC3339 → ms.
- **One assistant turn = up to 11 JSONL lines sharing one `message.id`, each repeating the *same* `usage`.** Naive per-line summing overcounts tokens **and cost up to 11×.** Collapse by `message.id`.
- **Per-agent attribution lives in `<session>/subagents/agent-*.jsonl` (+ `.meta.json`), not inline `isSidechain` in the parent.** It is exact and heuristic-free (verified). The original "attribute inline sidechain lines" mechanism had no inputs.

Plus: privacy claim was unenforced (now hardened), tps/LOC were oversold as benchmarks (now scoped + labeled), and the brain-as-only-hero was softened to **brain hero + a glanceable cost HUD on the same screen** (one decision left for you, §18).

---

## 1. Context — why this exists
Serious AI-coding now spans **multiple harnesses** (Claude Code, Codex, OpenCode…) × **multiple models** (Opus/Sonnet/Haiku/Fable, GPT, local Qwen via Ollama). Each writes rich telemetry to disk, but there's **no unified, visual, local** way to see what's happening: which harness/agent/model is working, how fast, at what cost, on which project, producing how much code.

- **ccusage** — local token/cost for Claude Code only; CLI tables; no cross-harness, no per-agent, no visuals. **(The real incumbent; it's `npx`-instant — acknowledge that our "download a binary" is heavier, and win on cross-harness + attribution + visuals.)**
- **Langfuse / Helicone** — powerful but app-level (not harness-aware) and heavy to self-host (Docker+Postgres+Clickhouse).
- **Grafana+OTEL** — Claude-Code-only, multi-service.

**Axon's wedge (in order of durability):** (1) **cross-harness cost + per-named-agent attribution** in one local pane; (2) **budget/spend awareness** (the daily-return hook); (3) a **beautiful live brain** that makes it shareable. The brain is the magnet; cost + attribution are the moat.

---

## 2. Locked product decisions
| Decision | Choice |
|---|---|
| **Form factor** | Single Rust binary → localhost web UI, opens the browser. macOS (arm64/x64) + Linux. |
| **Architecture** | **Self-contained**: own ingestion + embedded SQLite + embedded UI. OTEL/Langfuse = optional export sink (deferred, §14), never a dependency. |
| **Harness coverage (v1)** | Claude Code + Codex + OpenCode. (Ollama only *via* OpenCode.) **Claude verified; Codex/OpenCode must be fixture-verified before M2 — see §6.** |
| **Hero** | A live three.js "brain" **plus** a glanceable cost/budget HUD on the same landing ("Mission Control"). Brain nodes default = models; **size = cost** (not tokens); **stable radial layout**; agents/projects available as node-lenses. (One open decision, §18.) |
| **Leaderboards / social** | **Local-only.** Rank *your* models/agents/projects/days + a personal **Hall of Fame**. Share = exported **PNG card / WebM clip**. **No server, no P2P** (see §17). |
| **Benchmarks** | **Comparative stats from real usage** (cost-efficiency, LOC/€, cache-hit%; tps **labeled approximate**, not a ranked benchmark — §8). Passive only; never executes models. |
| **Frontend** | **Vue 3 + TresJS** + Vite, built to static assets, **committed to the repo** and embedded via `rust-embed` (no Node at `cargo build` time — §13). |
| **Privacy** | 100% local, **enforced** (§16): bundled assets, loopback-only bind, no egress unless the user exports a file or sets `--otel`. |

**Dimensions:** `harness · project · agent · model · tokens(in/out/cache) · cost(€, computed) · time · tps(approx) · lines-edited`

---

## 3. Users
- **Multi-harness power user** (primary): 3–10 parallel agent sessions; wants one pane for live activity + spend + who's pulling weight.
- **Cost-conscious dev**: "what did this week cost by model/project? cheapest model per LOC?" + a budget alert.
- **Sharer**: posts a weekly "my agents wrote N LOC for €X · Qwen hit Y tps" card.
- **Optimizer**: uses comparative stats to pick model/agent per task.

---

## 4. Goals / Non-goals
**Goals:** one binary, no deps, start→browser→data already there · parse history **and** tail live · harness-agnostic via a common Event · exact per-named-agent attribution (Claude) · a beautiful, *truthful* brain · local leaderboards + cards + budget alerts.
**Non-goals (v1):** no hosted/global/P2P leaderboard, no accounts, no server · no executing models (passive only) · no agent control (read-only) · no mobile.

---

## 5. Architecture
```
 ~/.claude/projects/<sid>.jsonl ─┐  (+ <sid>/subagents/agent-*.jsonl + .meta.json)
 ~/.codex/sessions/**/*.jsonl    ┼─▶ ingest/parsers ─▶ normalize ─▶ Event ─┐
 ~/.local/share/opencode/{db,log}┘        ▲                                 │
                                          │ register watchers FIRST,        ▼
                                   record per-file offset, THEN      single-writer task
                                   boot-scan up to offset            ─▶ SQLite (WAL)
                                          │                                 │
                                   notify ── live tail (seek from offset)   │
                                          │                                 │
              axum @ 127.0.0.1:PORT ◀─────┴── /api/* (aggregates, source of truth)
                ├─ GET /events  (SSE: decorative live pulses ONLY)
                └─ GET /*       (embedded Vue/TresJS via rust-embed)
                      │ opens browser; HUD numbers come from /api, not SSE
```
**Ordering rule (fixes the scan/tail race):** register watchers → snapshot each file's byte offset → scan `[0, offset)` → live-tail reads `[offset, EOF)`. With `message.id`-keyed upserts (§7) any overlap is a harmless no-op.

---

## 6. Data sources — **VERIFIED** (Claude) / to-verify (Codex, OpenCode)

> Cost is absent in every source → always computed (§8). See Appendix A for real redacted lines.

### 6.1 Claude Code — VERIFIED on real logs ✅
**Layout (critical):** each session is a **file + a sibling directory**:
```
~/.claude/projects/<ENCODED_CWD>/
  <session-uuid>.jsonl                       ← main thread
  <session-uuid>/subagents/agent-<id>.jsonl  ← each sub-agent run (isSidechain:true)
  <session-uuid>/subagents/agent-<id>.meta.json
  <session-uuid>/tool-results/…              ← offloaded large tool outputs (DON'T recount tokens)
  <session-uuid>/workflows/…                 ← Workflow-tool runs
```
(`ENCODED_CWD` = abs cwd with `/`→`-`.)

**Main-thread line (one per content block):**
- `type`: `"assistant"|"user"|"system"|"attachment"|"queue-operation"`.
- `timestamp`: **ISO-8601 string** (`"2026-06-14T10:21:18.279Z"`) — present on assistant/user/system/attachment/queue lines, **absent on the leading summary line**. Parse RFC3339→epoch-ms.
- `message.id` (e.g. `msg_01DZ…`) + `requestId` — **the turn key.** One turn is emitted as **2–11 lines sharing this id**, each `nblocks:1`, **each repeating the identical final `usage`**. → **collapse by `message.id`: take `usage` ONCE; union all lines' `content[]` blocks.**
- `message.model`: `claude-opus-4-8` | `claude-sonnet-4-6` | `claude-haiku-4-5` | `claude-fable-5`.
- `message.usage`: `{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, cache_creation:{ephemeral_1h_input_tokens, ephemeral_5m_input_tokens}, service_tier, speed}` (`speed` is categorical `"standard"`, **not** numeric — do not use as tps). `iterations[]` duplicates the same numbers — ignore it.
- `message.content[]`: `{type:"tool_use", id:"toolu_…", name, input}` where `name` ∈ `Read|Edit|Write|MultiEdit|Bash|Agent|Skill|WebSearch|…`. `uuid`/`parentUuid` per line.
- `isSidechain`: **always `false`** in the main file (confirmed).

**Sub-agent attribution — EXACT, no heuristics (verified):**
- `<sid>/subagents/agent-<agentId>.meta.json` = `{"agentType":"security","description":"…","toolUseId":"toolu_…"}`. **`agentType` = the roster name; `toolUseId` = the parent `Agent` tool_use `id`** → clean join to the spawning turn.
- `agent-<agentId>.jsonl`: `isSidechain:true`; **its own `message.usage` + `model`**; `agentId` (=filename); `attributionAgent` (often = agentType); `sessionId` (= **parent** session); `sourceToolAssistantUUID` (→ parent assistant uuid). Same `message.id` multi-line rule applies — **collapse here too**.
- **Attribution algorithm:** for each subagent file → read `.meta.json.agentType` → sum its own (deduped) usage → attribute to that named agent. The parent's `Agent` tool_use line does **not** carry the child's tokens (no double-count). Total session cost = main turns + Σ subagent turns.
- Parent `subagent_type` values seen in the wild: `architect, coder, security, Explore, claude-code-guide, general-purpose` (and `null` for some plain `Agent` calls — fall back to `agentType` from meta, else `"unknown-sub"`).

**Per-metric derivability:** model ✅ · tokens ✅ (after collapse) · cost ✅ (computed) · time ✅ (ISO) · tps ⚠️ approx (§8) · session/project ✅ · **named agent ✅ (exact, via subagents/+meta)** · skill ✅ (`Skill` tool_use → `input.skill`) · LOC ✅ (edit tools, §8 rules).

### 6.2 Codex — provisional (VERIFY before M2)
`~/.codex/sessions/{YYYY}/{MM}/{DD}/{session}.jsonl`. Recon (unverified): `model_provider`+id, `tokens.{input,output}`(+reasoning), `payload.timestamp` (ISO), `payload.id`. Agent = `"codex"`. LOC from `apply_patch`. **Action: open 2–3 real files, confirm the per-call id, whether turns split across lines (like Claude), and the timestamp format. Do not assume.**

### 6.3 OpenCode — provisional (VERIFY before M2; decide source now)
`~/.local/share/opencode/log/*.log` (key=value: `model={id,providerID}`, `tokens={input,output}`, `cost=`, `timestamp=ISO`, `id=ses_…`, `agent=<label>`) **and** `~/.local/share/opencode/opencode.db` (SQLite). **Decision: prefer `opencode.db` if its schema is stable; log as fallback.** Ollama appears as `providerID=…ollama` (cost 0, real tps). Agent = OpenCode label.

### 6.4 Ollama standalone
No local persistence — only via OpenCode. **No separate parser.**

---

## 7. Normalized Event (corrected)
```rust
struct Event {
  id: String,            // idempotent key — NOT a positional index:
                         //   Claude main: hash(harness, session_id, message_id)
                         //   Claude sub : hash(harness, agent_id,   message_id)
                         //   Codex/OpenCode: hash(harness, session_id, <verified per-call id>)
  ts: i64,               // epoch ms — PARSED from ISO-8601 in normalize.rs (no source emits epoch)
  harness: Harness,      // ClaudeCode | Codex | OpenCode
  project: String,       // git toplevel (see §8) ; fallback = cwd
  agent: String,         // Claude: agentType from .meta.json | "main" ; Codex: "codex" ; OpenCode: label
  is_subagent: bool,     // true for subagents/* turns
  model: String,         // raw id → canonical key (Appendix B map)
  session_id: String,
  tokens_in: u64, tokens_out: u64,
  cache_read: u64,
  cache_write_5m: u64, cache_write_1h: u64,   // kept SEPARATE — billed at different rates (§8)
  duration_ms: Option<u64>,                   // first→last ts within message.id, else next-turn delta; nullable
  loc_added: u32, loc_removed: u32,           // "lines edited", not committed diff (§8)
  loc_failed: bool,                           // true if any edit this turn errored (excluded from headline)
  skills: Vec<String>,
  cost_eur: f64,                              // computed at ingest; 0.0 for local models (flag separately)
  unpriced: bool,                             // model id not in pricing map → cost is a floor, surface loudly
}
```
**Collapse rule (MANDATORY):** one Event per `message.id` (main) / per `(agent_id, message.id)` (sub): take `usage` once, union `content[]` across the duplicate lines.

---

## 8. Cost · LOC · tps (corrected)
- **Cost** = `input·r_in + output·r_out + cache_read·r_cr + cache_write_5m·r_cw5 + cache_write_1h·r_cw1`, rates per-1M from `pricing.toml` keyed by canonical model id (Appendix B). **Cache *read* ≪ input ≪ cache *write*** — never collapse the buckets. Local models = 0 but `unpriced=false`. **A model id missing from the map ⇒ `unpriced=true` and is surfaced LOUDLY** (count + names in the UI), never a silent €0. Display €; `pricing.toml` carries an optional FX + a `priced_as_of` date shown in the UI.
- **LOC** = lines added/removed from `Edit/MultiEdit/Write/apply_patch` inputs, **counted once per `message.id`**, **only when the `tool_result` succeeded** (parse the result; skip errored/rejected edits → set `loc_failed`). `Write` over an existing file is **not** "all lines new" — diff against prior known content or label as rewrite. **Everywhere it appears, label it "lines edited by agent (≠ committed diff)."** Exclude obvious non-code extensions from the headline number (configurable).
- **tps** = `tokens_out / (clean_duration_s)`. Inter-turn intervals include think/tool/queue time → **only compute on turns with no intervening tool_use and no parallel sub-agent**, and **label "approx"** everywhere. **Not** a ranked leaderboard metric. (If Codex/OpenCode log real generation latency, prefer it.)
- **Ratio leaderboards** (LOC/€, $/1k): **exclude or bucket local/free models** (guard ÷0; otherwise local always "wins").

---

## 9. Backend (Rust)
**Crates:** `tokio`, `axum`+`tower-http`, `rust-embed`, `notify`, `rusqlite{bundled}`, `serde`/`serde_json`, `clap`, `tracing`, `webbrowser`; optional `opentelemetry-otlp` (HTTP+`rustls`, **never** gRPC — musl) behind `--features otel`.
**Modules:** `ingest/{mod,claude,codex,opencode,loc}.rs`, `normalize.rs`, `pricing.rs`, `model.rs` (Event/Harness + canonical map), `store.rs`, `watch.rs`, `server.rs`, `export/otel.rs`, `main.rs`.
**Concurrency (mandatory contract):**
- SQLite: `PRAGMA journal_mode=WAL; synchronous=NORMAL; busy_timeout=5000;`. **All writes through one `mpsc`→writer task**; readers use a separate pool. Boot scan upserts in ~1k-row transactions.
- `parser → bounded channel → writer → broadcast`. Boot scan can saturate the writer without blocking serve.
- **`notify` is lossy & fires mid-write:** on each event, `stat`; if `len < saved_offset` ⇒ rotation/truncation, reset to 0; read to EOF; **buffer the trailing partial line**, parse only `\n`-terminated lines. Persist `files(path, inode, offset, size)` in SQLite → restart resumes, not rescans.
- **SSE is decorative & lossy** (`broadcast` drops for slow receivers). Pulses may be dropped (invisible). **All numbers come from `/api/*` (SQLite = source of truth); the HUD polls/refetches, never accumulates from SSE.** Coalesce SSE→render to `requestAnimationFrame`.

**Routes:** `/` (assets) · `/api/summary` · `/api/leaderboard?by&metric&range` · `/api/timeseries` · `/api/models` · `/api/agents` · `/api/session/{id}` · `/api/agent/{name}` · `/api/budget` · `/events` (SSE) · `/api/card` (render share-card). **Bind `127.0.0.1` + `[::1]` only; reject foreign `Host`/`Origin` on `/api` + `/events` (§16).**

---

## 10. Frontend (Vue 3 + TresJS)
**Stack:** Vue 3 + Vite + TresJS(`@tresjs/core`,`cientos`,postprocessing) + Pinia + `vue-echarts` + native `EventSource`. **Every asset bundled (no CDN) — §16.**

### 10.1 Landing = "Mission Control" (brain hero + numbers)
- **Brain** (signature visual): nodes = **models**; **size = cost** (default; toggle to tokens); **color = harness**; **stable radial layout** (vendors as arcs, models orbiting) — *not* force-directed (jiggles at ~6 nodes). Node-lens toggle: recolor/regroup by **agent** or **project** (this is where the attribution USP becomes visual). On each SSE pulse the active node flares (bloom) + a capped, instanced particle burst (sized by tokens). Ambient shimmer when idle. **Perf gates (M4 acceptance): fixed particle pool (recycle oldest), bloom-resolution clamp, `visibilitychange` pause, rAF-coalesced SSE → 60fps on integrated GPU.**
- **HUD on the same screen** (the daily-driver half): today's spend **vs budget**, live aggregate tps (approx), active sessions/agents, top model+agent today. This is what makes it useful past the novelty.

### 10.2 Analytics
Cost over time (stacked by model/harness), tokens (in/out/cache stacked), LOC over time, cache-hit %, activity heatmap, spend by project. Range-filterable.

### 10.3 Leaderboards + Hall of Fame (local)
Ranked by **model** and **agent**: **spend** and **LOC/€** in v1 (defer the other metrics — YAGNI). Personal **Hall of Fame** = your all-time bests (compete against yourself; retention without trust cost). Share-card/clip from here.

### 10.4 Drill-down
Per agent + per session: turns, model(s), tokens/cost/LOC/tps, skills, project, branch.

### 10.5 New capabilities (the retention engine — were missing)
- **Budget / cost-cap alerts**: set €/day or €/week; HUD shows "€14 of €20"; toast at threshold. *(Highest-value addition.)*
- **CSV/JSON export** of aggregates (table-stakes for expensing; ccusage has `--json`).
- **End-of-session nudge**: "session done: €0.42, 1.2k lines edited, Opus 80%."
- **Tray/menubar live indicator** (post-v1, but design for it): €today + tps, click→open.
- **Model recommendation** (post-v1): "Sonnet did your edit-heavy turns at ⅕ of Opus's cost."

### 10.6 Growth loop (specify it — it's the only organic channel)
Share-card MUST carry a **"made with Axon ↗" wordmark** (that's the loop), show a flex stat + period + tiny brain thumbnail, and **redact project/branch names by default** (§16). **WebM clip = the primary share asset** (motion stops the scroll); PNG fallback. Add an **auto weekly recap** card.

---

## 11. Stack summary
Rust: axum·tokio·rust-embed·notify·rusqlite(bundled)·serde·clap·webbrowser·(opt)opentelemetry-otlp[http,rustls]. UI: Vue3·Vite·TresJS·Pinia·vue-echarts·EventSource. Store: SQLite `~/.local/share/axon/axon.db` (mode `0600`, dir `0700`). Config: `~/.config/axon/{config.toml,pricing.toml}`.

---

## 12. Repo layout
```
axon/
├── Cargo.toml
├── src/{main,server,store,watch,pricing,model,normalize}.rs
│   └── ingest/{mod,claude,codex,opencode,loc}.rs   + export/otel.rs
├── ui/                  # Vue+TresJS (Vite)
│   └── dist/            # BUILT + COMMITTED → rust-embed input (no Node at cargo build)
├── assets/pricing.toml  # bundled defaults + priced_as_of date
├── tests/fixtures/{claude_main.jsonl, claude_subagent.jsonl, claude_subagent.meta.json, codex.jsonl, opencode.log}  # see Appendix A
└── DESIGN.md            # this file
```
**CI** builds `ui/` and commits/vendors `ui/dist`; `build.rs` only embeds (so `cargo install axon` works with no Node — §13).

---

## 13. Build & distribution
Cross-compile `aarch64/x86_64-apple-darwin`, `x86_64-unknown-linux-{gnu,musl}` (musl=static; pairs with bundled SQLite). GitHub Actions matrix → release binaries + curl-pipe installer · `cargo install axon` · Homebrew tap. **OTEL feature off by default** (keeps the static musl build clean).

---

## 14. Milestones
1. **M1 — Claude ingest + store (headless), incl. sub-agent attribution.** Parse main + `subagents/` (collapse by `message.id`; ISO ts; cost; LOC; agentType). `--scan-only` emits JSON. **Gate (§15): a committed real-subagent fixture attributes tokens to the right `agentType`, and total tokens match a hand-checked turn (no 11× inflation).**
2. **M2 — Codex + OpenCode.** *First* verify each against real files (§6.2/6.3) + commit fixtures; then parsers; canonical model map; € cost; unpriced surfaced loudly; ratio-leaderboard ÷0 guard.
3. **M3 — Server + analytics UI.** axum (loopback+Origin) + `/api/*` + embedded Vue; analytics + leaderboards + Hall of Fame + drill-down + **budget alerts** + CSV/JSON export (no 3D yet).
4. **M4 — Brain + live.** WAL/writer/offset pipeline; `notify`→SSE (decorative); Mission-Control landing (brain + HUD); perf gates met.
5. **M5 — Cards/clips + packaging.** Wordmarked PNG/WebM + weekly recap + redaction; pricing override; cross-compiled releases + installer + tap. **Rename verified everywhere = `axon`.**
6. **M6 (optional) — OTEL export** behind the flag, with field-redaction (§16).

---

## 15. Verification (expanded — §15 of the old draft was too trusting)
1. **Timestamp**: parser converts a real ISO line to epoch-ms; rejects/flags a numeric one.
2. **Collapse/no-double-count**: a fixture turn with 11 same-`message.id` lines yields ONE Event with the single true `output_tokens` (not 11×).
3. **Sub-agent attribution (M1 GATE)**: real `subagents/agent-*.jsonl`+`.meta.json` → tokens attributed to `agentType`; report the **% of tokens that fell back to `"main"/unknown"`** (honesty gauge).
4. **Idempotency**: full-scan vs delta-tail vs re-scan ⇒ identical row set (key on `message.id`/`agent_id`, never positional). Include a resumed/forked-session fixture.
5. **Cost golden**: all four token buckets → €-to-the-cent; assert cache-read ≠ cache-write rate; an unpriced-model fixture is flagged loudly, not €0-silent.
6. **LOC truth**: failed/rejected edit not counted (or counted+flagged); `Write`-over-existing not counted as all-new.
7. **Zero-outbound**: run with no `--otel` under a network sandbox; assert **zero** outbound sockets across boot+UI+SSE. **Build-time grep fails on any `http(s)://` literal in `ui/dist`** (CDN/font/HDRI).
8. **Bind/Origin**: server binds `127.0.0.1` (not `0.0.0.0`); foreign `Host`/`Origin` to `/api`,`/events` rejected.
9. **Card redaction**: exported card/clip contains no project path, repo, or branch unless explicitly opted in.
10. **Single-binary**: runs on a clean machine (no Node/toolchain).
11. **Cross-check**: Claude totals within rounding of `ccusage` for the same window.

---

## 16. Privacy — make "100% local" an enforced property, not a slogan
- **Bundle every UI asset** (three.js/TresJS/ECharts/fonts/HDRIs/textures) into `rust-embed`; **no CDN `<script>`/`@import url(https://…)`/remote env-maps.** Build-time grep gate (§15.7).
- **Bind loopback only** (`127.0.0.1`/`[::1]`); **anti-DNS-rebind/CSRF**: reject non-`localhost` `Host` and cross-origin `Origin`/`Referer` on `/api`+`/events` (there is no auth — loopback + origin checks are the boundary).
- **SQLite `0600`, dir `0700`** (it holds your full cross-project history — PII-grade).
- **Share-cards/exports**: strip/aggregate project + **branch** names by default (branches leak ticket/customer codenames); real names = explicit per-export opt-in with a warning.
- **OTEL (`--otel`)**: document exactly which fields leave (project, branch, session, agent); offer hashing/allowlist; never auto-enable.

---

## 17. The P2P / "anyone-can-host" global leaderboard — decision: **NO (v1), with a documented path**
- **Transport: feasible & genuinely torrent-like.** `rust-libp2p` GossipSub (fan-out) + Kademlia DHT (discovery) + signed append-only/CRDT records + baked-in bootstrap multiaddrs (= "anyone can host a seed node"). This is the easy 20%.
- **Trust: the fatal 80%.** Every metric derives from logs the user fully controls and can edit. A signed P2P record proves *"this key claims X,"* never that X happened — **signing = origin, not truth.** "Anyone can join" ⇒ free identities ⇒ **Sybil-farmable.** Unsolvable without a **trusted attester** (the token issuer signing usage receipts), which **doesn't exist** today and a read-only local tool can't mint. ZK doesn't help (proves correct computation over an input, not that the input is real); PoW/stake taxes spam without making a forged number true. A global board also **breaks the "100% local" positioning**, needs accounts (a non-goal), and needs moderation.
- **Ship instead:** local **share-cards + weekly recap** (the flex, zero trust) and a local **Hall of Fame** (compete vs yourself). *If* a social board is ever wanted, it's a **separate, opt-in, self-hostable instance you run** with server-side verification — **never** the local binary phoning home, **never** P2P (multiplies the trust problem with no arbiter), **never** marketed as cheat-resistant.

---

## 18. Open items / decisions for you
1. **Name = `Axon`?** (Renamed off `cortex` for the Jan/Menlo collision.) Confirm or pick `Pulse`/`Synapse` before M1 names the crate.
2. **Hero — one call:** this spec keeps your **brain as the literal landing**, paired with a cost/budget HUD on the same screen (Mission Control), nodes sized by **cost**, stable layout. The product reviewer argued for an analytics-first landing with the brain one tap away. **Default = your brain-hero (kept); flip to analytics-first only if you want.**
3. **Codex/OpenCode**: budget the §6.2/6.3 real-file verification into M2 (don't trust the recon — Claude's recon was wrong).

---

## Appendix A — verified fixture shapes (redacted; save under `tests/fixtures/`)
*Real structure from this machine; content redacted to `…`. These are the M1/M2 test inputs.*

**A1 · Claude main turn — 2 of the 11 same-`message.id` lines (collapse → ONE Event, usage taken once):**
```jsonl
{"type":"assistant","timestamp":"2026-06-14T10:21:18.279Z","sessionId":"<sid>","cwd":"/abs/repo","gitBranch":"main","isSidechain":false,"uuid":"u1","parentUuid":"u0","requestId":"req_…","message":{"id":"msg_01DZ…","model":"claude-opus-4-8","stop_reason":"tool_use","content":[{"type":"tool_use","id":"toolu_A","name":"Edit","input":{"file_path":"…","old_string":"…","new_string":"…"}}],"usage":{"input_tokens":11341,"output_tokens":10498,"cache_read_input_tokens":0,"cache_creation_input_tokens":29928,"cache_creation":{"ephemeral_1h_input_tokens":29928,"ephemeral_5m_input_tokens":0},"speed":"standard"}}}
{"type":"assistant","timestamp":"2026-06-14T10:21:23.404Z","sessionId":"<sid>","isSidechain":false,"uuid":"u2","parentUuid":"u1","message":{"id":"msg_01DZ…","model":"claude-opus-4-8","content":[{"type":"tool_use","id":"toolu_B","name":"Agent","input":{"subagent_type":"security","description":"…"}}],"usage":{"input_tokens":11341,"output_tokens":10498,"cache_read_input_tokens":0,"cache_creation_input_tokens":29928}}}
```
*(Both lines: same `msg_01DZ…`, identical `output_tokens:10498`. Event = 1, out=10498, content = union of the two tool_use blocks.)*

**A2 · Sub-agent meta + line (attribution join: `meta.toolUseId` == the parent `Agent` tool_use `id`):**
```
// agent-a14f….meta.json
{"agentType":"security","description":"Data-correctness + privacy review","toolUseId":"toolu_B"}
```
```jsonl
// <sid>/subagents/agent-a14f….jsonl  (isSidechain:true; OWN usage; sessionId = parent)
{"type":"assistant","timestamp":"2026-06-15T10:58:…Z","isSidechain":true,"agentId":"a14f…","attributionAgent":"security","sessionId":"<sid>","sourceToolAssistantUUID":"u2","message":{"id":"msg_subX","model":"claude-sonnet-4-6","content":[{"type":"text","text":"…"}],"usage":{"input_tokens":…,"output_tokens":850,"cache_read_input_tokens":…}}}
```

**A3 · Codex / OpenCode**: capture real samples during M2 verification and add here.

## Appendix B — canonical model-id map (seed; extend in `pricing.toml`)
| raw id (examples) | canonical | priced |
|---|---|---|
| `claude-opus-4-8` | `claude-opus-4.8` | yes |
| `claude-sonnet-4-6` | `claude-sonnet-4.6` | yes |
| `claude-haiku-4-5` | `claude-haiku-4.5` | yes |
| `claude-fable-5` | `claude-fable-5` | yes |
| `gpt-5.5*` (Codex) | `gpt-5.5` | yes |
| `…ollama/<m>`, local Qwen (OpenCode) | `local:<m>` | **0 (free, not "unpriced")** |
| anything else | passthrough | **`unpriced=true` → surface loudly** |
*Rule: normalize raw→canonical; unknown ⇒ unpriced (cost is a floor), never silent €0.*

---

## Next step
Copy to `axon/DESIGN.md`; M1 first (Claude ingest incl. sub-agents → SQLite → `--scan-only`, with the Appendix A fixtures as the test gate).
