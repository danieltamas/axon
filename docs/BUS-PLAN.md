# PLAN — axon-bus: the control plane for Axon (open source, by Daniel Tamas)

## 00. Model and prior art (2026-09-29, decided)

**The model (the user's, and binding): the harnesses talk to a message hub that may or may not exist.**
- **Plug-in, not wrapper.** Hooks are installed once in each harness's own config, and harnesses are launched exactly as they are today. Nothing runs in front of them.
- **Hub absent ⇒ vanilla harness.** Each hook first stats `axon.db` (or `serve`'s pidfile). If it is missing, the hook exits 0 with no output, within 1 ms (p95) of the host's process-spawn floor (`/usr/bin/true`; spike Q4). Native features such as Claude `SendMessage` keep working untouched.
- **Hub present ⇒ policy, audit, delivery.**
  - Native messaging stays the transport where one exists (Claude `SendMessage`, Codex `turn/steer`, OpenCode `prompt_async`). The hub adds a `PreToolUse` check on `SendMessage` that denies any non-edge send, and a `PostToolUse` log entry.
  - Only harnesses without a native channel (Gemini, Copilot, Kimi, Hermes) get hook injection (`AfterTool` / `PostToolUse` `additionalContext`).
- **Hub removed mid-session ⇒ same as absent.** Pending stops still fail closed through the doorbell file (§6).

**Prior art, rejected as the base: hcom** (`github.com/aannoo/hcom`, MIT, Rust core distributed via uv/brew, v0.7.26). It is a **wrapper**: "hooks activate only when an agent is launched with `hcom` in front" (`hcom claude`). It also delivers by typing into terminals, has no parent/child tree, no routing restriction ("no scoped roles"), no cost tracking and no audit log. The mismatch is its model, not its quality. Ideas worth borrowing: `events --wait` filters, collision events, and its per-harness hook notes as a reference while writing adapters.

**Spike (one session, before M0), Claude + Gemini only:**
1. Can a process outside a Claude session post into that session's `SendMessage` socket? This is undocumented. If not, cross-harness → Claude goes through hook injection.
2. Does a `PreToolUse` hook matching `SendMessage` see the target and body, and can it deny the send?
3. Does Gemini's `AfterTool` `additionalContext` reach the model mid-turn? Does `BeforeTool` deny work?
4. Does the hub-absent fast path stay ≤ 2 ms p95 (hyperfine, 500 runs)?

Record the results as a table here, with commands and output.

**Spike results (2026-09-29, Claude Code 2.1.284, Gemini CLI 0.61.0, macOS arm64; scripts in the session scratchpad, nothing in `src/`):**

| # | answer | command | output (trimmed) |
|---|---|---|---|
| 1 | **No, not as a plug-in.** There is no documented socket. Each live session registers `~/.claude/sessions/<pid>.json` next to a `<pid>.<sha256>.key` secret, and no `claude` process holds a TCP listener. The only Unix sockets seen are the daemon's PTY sockets (`/tmp/cc-daemon-501/…/*.pty.sock`). Posting into a session would mean reading its private key, which a hub must not do. The probe stopped at that point (the permission classifier denied reading session records). **Cross-harness → Claude goes through hook injection.** | `ls ~/.claude/sessions \| sed … \| uniq -c`; `lsof -nP -iTCP -sTCP:LISTEN`; `lsof -U -a -c claude` | `51 .<sha256>.key` / `51 .json`; no claude TCP listener; only `…/spare/a5962e70.pty.sock` |
| 2 | **Yes on both.** A `PreToolUse` hook with matcher `SendMessage` receives the target and the body. `permissionDecision: "deny"` blocks the send, and the model sees the reason as the tool result. The payload has `session_id` and `transcript_path` but **no sender agent name**, so the sender comes from the registry (`session_id` → agent). | `claude -p --model haiku --settings q2-settings.json 'ToolSearch select:SendMessage, then SendMessage to="orch2" message="spike ping 42"'` (hook: log stdin, print deny JSON) | hook stdin: `"tool_name":"SendMessage","tool_input":{"to":"orch2","message":"spike ping 42","recipient":"orch2","content":"spike ping 42",…}`; model's report: `axon-bus spike: non-edge send denied, route is orch1 -> orch2`; cost $0.052 |
| 3 | **Yes on both, verified in source; the live run is blocked on auth.** In `bundle/chunk-Y2J3C62M.js`: AfterTool `hookSpecificOutput.additionalContext` is appended to the tool result's `llmContent` as `<hook_context>…</hook_context>` (L345062–79), so it reaches the model mid-turn in that tool result. BeforeTool `decision: "deny"\|"block"` (L337858) returns `Tool execution blocked: <reason>` without running the tool (L344973–82). The live check needs a Gemini credential, and this Mac has none. | `grep -n additionalContext\|isBlockingDecision bundle/*.js`; live: `gemspike/run.sh` (AfterTool on `read_file` injects a code word, BeforeTool on `run_shell_command` denies) | source lines quoted above; live: `exit=41 … "Please set an Auth method … GEMINI_API_KEY, GOOGLE_GENAI_USE_VERTEXAI, GOOGLE_GENAI_USE_GCA"` |
| 4 | **No, ≤ 2 ms p95 cannot be met by any exec'd hook here.** On this host, spawning a process at all costs more: `/usr/bin/true` measures 2.46 ms p50 and 6.51 ms p95. A stripped Rust stand-in (`stat $XDG_DATA_HOME/axon/axon.db`, then exit 0 silently) is at that floor, and `sh -c test -e` is about twice as slow. An unsandboxed re-run gave the same picture, so the sandbox was not the cause. There, the stand-in is +0.33 ms p50 and +0.32 ms p95 over the floor, within the new 1 ms target. | `hyperfine -N --warmup 20 --runs 500 fastpath "/bin/sh -c 'test -e …/axon.db \|\| exit 0'"`; `hyperfine -N --runs 500 /usr/bin/true` | fastpath p50 2.91 / p95 5.85 / max 25.7 ms; sh p50 6.05 / p95 13.27 ms; true p50 2.46 / p95 6.51 ms. Unsandboxed (`q4-unsandboxed.sh`): fastpath p50 2.89 / p95 6.23; true p50 2.56 / p95 5.91 ms |

**What changes as a result:**
- §3 drops the Claude "`SendMessage` socket mirror": idle wake for Claude is delivery at the next prompt or tool call only.
- §3 enforces edges on Claude's native sends with `PreToolUse(SendMessage)`, keyed by `tool_input.to` plus `session_id` → registry.
- The hub-absent target (§00, M1) becomes "≤ 1 ms above the `/usr/bin/true` floor on the same host, p95", in place of an absolute 2 ms.
- Gemini CLI moves from v2 to v3, and Codex holds v2 alone (below). Q3's live Gemini run stays open until a Gemini API key exists.

**Gemini demoted (2026-09-29).** Signing in to Gemini CLI 0.61.0 with a Google account fails with: "This client is no longer supported for Gemini Code Assist for individuals … please migrate to the Antigravity suite of products". Only API-key or Vertex users can still run it, so it is a shrinking target. Antigravity, where those users are sent, has no hook primitive. Codex has native push (`turn/steer`), so it takes the v2 slot.

**Harness rollout (v1 is Claude only; adapters sit behind one `Harness` trait):**

| harness | hooks: register / deliver / deny | native push | order |
|---|---|---|---|
| Claude Code | SessionStart / PostToolUse `additionalContext` / PreToolUse | `SendMessage`, in-harness only (not reachable from outside; spike Q1) | **v1** |
| Codex CLI | SessionStart, SubagentStart / PostToolUse / PreToolUse | app-server `turn/steer` | v2 |
| Gemini CLI | SessionStart / AfterTool `additionalContext` (appended to the tool result) / BeforeTool `decision: deny`; `session_id` + `transcript_path`; **no subagent id**. API-key or Vertex auth only (Google-account sign-in retired) | none documented | v3 |
| OpenCode | plugin `session.created` / `tool.execute.after` / `tool.execute.before` throw | `prompt_async` | v3 |
| Copilot CLI | `sessionStart` / `postToolUse` / `preToolUse` deny (`~/.copilot/hooks/*.json`) | none documented | v3 |
| Kimi Code | 13 events incl. SessionStart / PostToolUse / PreToolUse (`~/.kimi/config.toml`, hooks in beta) | none documented | v3 |
| Hermes | `on_session_start` / `pre_llm_call` / `pre_tool_call` | none | v3 |
| Antigravity | **no hook primitive**: conversations are protobuf files in `~/.gemini/antigravity/conversations/` | none | observe-only (Axon ingest) |
| Superset CLI | the host that launches the harnesses above, not a model harness | — | reads the registry only |

## 0. Decision: build on Axon, not beside it (2026-09-29)

`github.com/danieltamas/axon` (local copy `~/Sites/axon`, MIT, v0.2.1, clean tree, last commit 2026-06-20) already does about half of this plan in the same stack: Rust, axum, rusqlite in WAL mode, and an embedded single-file dashboard.

| plan need | Axon today | action |
|---|---|---|
| usage tailers per harness | `src/ingest/{claude,codex,opencode,ccflare}.rs`, `notify` live-tail with byte offsets, race-free boot scan | **reuse**; add `hermes.rs` |
| exact per-subagent attribution | `parse_subagent_jsonl` (Claude `subagents/agent-*.jsonl` + `.meta.json`) | **reuse** |
| prices, real vs notional | `pricing.rs` + `pricing.toml`; the `PricingKind` values `api-money`, `chatgpt-included`, `reported-cost` and `local-free` are exactly the two-currency split | **reuse** |
| dashboard server security | `server.rs`: bind 127.0.0.1, reject non-loopback Host and cross-origin Origin | **reuse**; add a per-boot token for POSTs (Axon is read-only today) |
| budgets | day/week/month EUR **alerts** (`config.rs`) | **extend** to per-tree ceilings with enforcement |
| fixture tests | `tests/m1..m3_fixtures.rs` | **extend** |
| releases | `bin/` holds macOS universal and Linux x64/arm64 | **add** Windows x64 and move to cargo-dist |
| registry, hooks, messages, routing, stop gate, spawn, virtual agent, memory, live narrative | none | **new** |

**Shape:**
- The Axon repo becomes a Cargo workspace:
  - `axon-core`: `model`, `normalize`, `store`, `pricing` and `ingest`, moved and not rewritten.
  - `axon`: the existing observe-only dashboard binary.
  - `axon-bus`: the new control plane, a **pluggable addon**.

Why a separate binary rather than a cargo feature:
1. **Trust.** Axon's promise is read-only and zero-egress: it never touches your harness config. `axon-bus` writes hooks and can deny tool calls, so installing it has to be a separate, explicit act (`axon-bus install`).
2. **Hook latency.** `axon-bus hook` must start without a tokio runtime (≤ 10 ms p95). Axon's binary starts one.
3. **Graceful absence.** Both share `axon.db` (the bus adds its own tables). The `axon` dashboard *detects* the bus tables and lights up the tree, chatter, controls and budget views; without them it is today's Axon.

Everything below is kept, with the crate and tool names mapped: `agent-bus` → `axon-bus`, `src/usage/*` and `src/prices.rs` → `axon-core` (reused), `src/serve.rs` → the existing `axon` server, extended.

A single binary that lets coding agents from different harnesses register, talk along fixed routes, and stay within a budget. A local dashboard shows every agent, its status, memory, tokens, cost and messages. It is plug'n'play on macOS, Linux and Windows.

- Status: **approved 2026-09-29.** All §12 items are decided; next is M0 (§11).
- Research and rationale: `improve/agent-bus-2026-09-29.md`.
- Home: `docs/BUS-PLAN.md` in the Axon repo (moved 2026-09-29). Research: `~/agent-os/improve/agent-bus-2026-09-29.md`.

## 1. Complete when (one sentence a stranger can check)

On a clean machine, the release installer puts `agent-bus` on PATH, and `agent-bus install` wires every harness it detects.

Then a real Claude Code orchestrator with 3 subagents and a real Codex orchestrator with 2 subagents run under a 2M-token budget, and the dashboard at `http://127.0.0.1:7433` shows the following live:
- both trees, each node marked idle or active, with its memory, tokens and cost;
- the messages between them;
- a question that is relayed and answered;
- the budget stop that halts the remaining work.

Afterwards, `agent-bus audit --verify` and `cargo test` both pass on all three OS targets in CI.

## 2. Architecture: one binary, one database file, one page

```
 harness hooks ──stdin JSON──▶  agent-bus hook <harness> <event>  ──▶  SQLite (WAL, 1 file)
 (Claude, Codex, OpenCode,        normalise → act → print the         agents · edges · messages
  Hermes)                         harness-specific JSON reply         claims · usage · budgets
                                  (inject / deny / allow)             events (append-only, hash-chained)
 usage tailers (transcripts) ───────────────────────────────────────▶ usage
 agent-bus serve ◀──────────── reads the same DB ── SSE ──▶ dashboard (static, embedded)
```

**Principles:**
- **The database is the only state.** Every command is a short-lived process against one SQLite file.
- **`serve` is optional.** It is a reader plus a wake-up sender. Killing it loses nothing.
- **Hooks never hurt the host session.** Each hook has a 300 ms budget and SQLite `busy_timeout` is 150 ms. On any error the hook prints "allow", except the stop/budget gate, which fails closed (§6).
- **One normalized event model.** The per-harness code is limited to (a) turning the hook payload into an event, (b) turning a verdict back into the harness's reply format, and (c) reading usage from the transcript. Nothing else knows which harness it is talking to.

### Crate layout (one crate, each file ≤ 500 lines)

```
src/main.rs          clap dispatch only
src/store.rs         schema, migrations, append-only events + hash chain, busy_timeout
src/registry.rs      register / close / orphan sweep, status (active|idle|closed|orphaned), memory
src/route.rs         message edges, links, grants; `route` (task → harness/model/effort)
src/msg.rs           send / ask --wait / reply / inbox / doorbell files
src/claims.rs        path claims + overlap check (absorbs repo-sync's claim half)
src/budget.rs        per-tree and per-agent ceilings, 80% warn, 100% stop
src/usage/{claude,codex,opencode,hermes}.rs   incremental transcript tailers (byte offsets)
src/prices.rs        bundled prices.toml + `prices update` from OpenRouter /models
src/hook.rs          payload → event → verdict → per-harness reply JSON
src/install.rs       detect harnesses, wire hooks idempotently with backup, uninstall, doctor
src/spawn.rs         headless launch: claude -p · codex exec · opencode run (env carries parent/role/mission/budget)
src/virtual_agent.rs panel + judge node (§5)
src/serve.rs         axum: GET /api/snapshot, GET /api/stream (SSE), POST /api/msg; Host/Origin/token checks
ui/                  index.html · app.js · style.css · icons.svg · vendor/preact+htm (no build step, embedded)
tests/acceptance/    black-box CLI tests — authored by Codex from this plan, never edited by the implementer
tests/fixtures/<harness>/   real captured hook payloads and transcript slices
backtest/            replay corpus manifest + expected results
```

**Dependencies (the complete list):**
- `clap`, `rusqlite` (bundled), `serde`, `serde_json`, `toml`, `sha2`, `dirs`
- `sysinfo` (pid liveness and RSS on all three OSes, so Windows needs no `kill -0`)
- `ureq` (outbound HTTP: prices, OpenCode wake, optional Jev)
- `axum` + `tokio`, used only in `serve`

Nothing else without a written reason in the PR.

### Data model

| table | columns (all snake_case) |
|---|---|
| `agents` | `id, harness, session_id, agent_ref, pid, parent_id, root_id, role, mission, model, repo, cwd, worktree, status, started_at, last_seen_at, ended_at` |
| `edges` | `from_id, to_id, kind (tree\|link\|grant), thread, expires_at` |
| `messages` | `id, thread, seq, from_id, to_id, kind, body(≤400), refs_json, needs_reply, deadline, default_reply, delivered_at, acked_at` |
| `claims` | `agent_id, checkout, path, task, created_at` |
| `usage` | `agent_id, ts, model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, cost_usd, source_offset` |
| `budgets` | `scope_id, kind (tree\|agent), tokens_max, usd_max, state (ok\|warned\|stopped)` |
| `events` | `seq, ts, actor, verb, subject, payload_hash, prev_hash` (triggers reject UPDATE and DELETE) |

**Status is taken from hooks, not guessed:**
- `active` runs from UserPromptSubmit or SubagentStart until Stop.
- `idle` means the process is alive but no turn is running.
- `closed` comes from SessionEnd or SubagentStop.
- `orphaned` means the pid is dead with no close.

**Memory** is the RSS of the pid plus its descendants (MCP servers, node children), from `sysinfo`. Claude and Codex subagents run in their parent's process, so the UI shows them as "shared with parent" instead of inventing a number.

## 3. Message routing (deterministic)

- Edges are parent ↔ child, plus root ↔ root over a `link` that the other root must `accept`. A send to anything else is rejected, and the error prints the route to take (`orch1 → orch2 → sub2`). The intermediate decides whether to forward. `grant --thread --ttl` opens a logged temporary direct edge.
- **Claude native sends are policed, not replaced.** A `PreToolUse` hook matching `SendMessage` reads `tool_input.to` and resolves the sender from `session_id` → registry (the payload carries no sender name). A non-edge send is denied with the route in `permissionDecisionReason`, which the model sees as the tool result (spike Q2). `PostToolUse` logs each allowed send.
- The only fan-out is `sync`, to your own direct children. There is no broadcast.
- Message kinds are `question`, `answer`, `stop`, `redirect`, `sync`, `handoff` and `ack`. The body is capped at 400 characters, and content goes by reference (`path:L10-40@sha`).
- `ask --wait N --default "…"` blocks inside one tool call and returns the answer. The asker re-reads nothing.
- **Delivery:** the doorbell file is stat-checked after each tool call, with 0 tokens spent when empty. Unread messages are injected with an untrusted-peer frame. `stop` and a budget stop are enforced by denying the next tool call.
- **Idle wake** (`serve` only):
  - Claude: delivery at the next prompt or tool call. There is no external push, because a session's `SendMessage` channel can't be reached from outside (spike Q1).
  - OpenCode: `prompt_async {noReply:true}`.
  - Codex: `turn/steer` for app-server sessions.
  - Hermes: delivery at the next turn.

## 4. Task routing: which harness, model and effort a new agent gets

`agent-bus route "<task>" [--role R] [--budget B]` answers `{harness, model, effort, reason}`. `spawn --auto` uses the answer.

1. **Rules decide.** `routes.toml` maps role and tags to a lane, in the same shape as agent-os `ROUTING.md`. Every decision is logged with its reason, and the same input always gives the same output.
2. **Budget clamps the decision.** If the remaining tree budget cannot cover the lane's typical cost (the median from the `usage` history for that role), `route` steps down one lane and says so.
3. **Advisors only suggest.** With `advisor = "jev"`, Jev (typed probabilities, about 1K tokens per call) scores the spawn with the questions it already asks: does it need the caller's context, is it independent breadth, is it a review leg. With `advisor = "openrouter-auto"`, a task already in an OpenRouter lane (OpenCode) passes `openrouter/auto` as its model, and OpenRouter's router picks per request. Advisor output is logged next to the rule's answer as a shadow. It never overrides a rule until the backtest (§8) shows it agrees with the human labels better than the rule does.

## 5. Virtual agent (the agent-level equivalent of OpenRouter Fusion)

OpenRouter Fusion runs a panel of models and a judge that synthesises their answers into one. `agent-bus` makes that a node in the tree: `spawn --virtual --panel claude:opus,codex:gpt-5.5,opencode:gemini-3.1-pro --judge claude:opus "question"`.

- To its parent, it is **one addressable agent**. Inside, the panel runs headless in parallel as its children, and the judge child receives their answers by reference and returns one answer.
- Different vendors on the panel and a judge from a different vendor than the author keep the writer≠judge rule structural.
- Its cost is the subtree cost, so it counts against the budget like anything else.
- `--fusion` is a cheap text-only variant. It calls OpenRouter Fusion directly (one HTTP call, no harness processes), for questions that need no repo access.

## 6. Budgets

- `spawn … --budget 2Mtok|3usd`, or `budget set <root> …`, stores a ceiling on the tree. It is optional per agent as well.
- The cost of a tree is the sum of `usage` over it (tokens × `prices.toml`, plus Jev and Fusion calls).
- Every total is reported two ways: real money (API or OpenRouter billing) and a notional figure (subscription harnesses, priced at API rates).
- At **80%**, a `sync` goes to the root and the node turns amber on the dashboard. At **100%**, a `stop` goes to the whole subtree, and the gate denies the next tool call of each agent in it.
- **Fail closed:**
  - Usage data older than 120 s on an active tree with a budget → the gate warns once, then denies once the tree reaches 90% of its last known total.
  - An unreadable database while the doorbell shows a stop → deny.
  - An unreadable database with no doorbell → allow, because an unrelated session must never be bricked.
  - The acceptance tests cover every one of these branches.
- **Honest limit:** a budget stops *new tool calls*. It cannot cut off a model turn already streaming, so the overshoot is at most one turn. The dashboard shows that overshoot.

## 7. Dashboard

The dashboard is served by `agent-bus serve` and embedded in the binary. It needs no Node and has no build step.

**Views:**
1. **Live tree.** Every harness, agent and subagent, with harness glyph, model, role, mission, repo/worktree, status (active pulses, idle rests, orphaned gets a warning mark), memory, tokens, cost, and a budget ring.
2. **Chatter.** Messages as edges animated along their routes, and a thread timeline beside the tree. Selecting a thread shows its route, delivery and ack times, and its refs.
3. **Cost.** Per task tree: real vs notional spend, burn rate, budget remaining, and the cost split by harness and model.
4. **Human node.** Send `stop`, `redirect` or an answer. Each is logged like any other message.
5. **Live narrative (per agent).** Selecting an agent opens a stream of *what it is saying and thinking*, not what it is executing:
   - **Messages first.** Assistant text, bus messages, and spawn/hand-back prompts are shown in full as they are written. They are read by the same incremental transcript tailers as usage (`src/usage/*` → a shared `transcript.rs`), so no second parser exists.
   - **Reasoning, exactly as the harness recorded it.** Each block shows the text the harness writes to disk, labelled with its source, and nothing more. What was checked on 2026-09-29:
     - Claude Code: almost all `thinking` blocks carry only a signature with the text omitted (792 of 797 in a recent session). The few with text are short summaries.
     - Codex: reasoning is `encrypted_content`, plus 0–3 plaintext `summary` items.
     - A block with no readable text renders as a quiet "reasoning — not recorded by <harness>" row with its token count. There is no attempt to decrypt or reconstruct it; that content belongs to the provider.
     - When a harness records more (OpenCode reasoning parts, Hermes), the same row shows it.
   - **Tool noise collapsed.** Bash, Read, Grep, Glob and similar calls collapse into one chip per run ("7 reads · 3 commands · 2 edits"), which expands on click. Edits and writes show their paths. Failures stay visible.
   - **Privacy.**
     - Content capture is **opt-in** (`serve --content`, or `[content] enabled = true`). Without it, the stream shows the structure only: turn, token counts, tool chips.
     - Content lives only in the local database, is redacted for secret patterns before storage, has a retention default of 7 days, and is never exported by `replay` or the backtests.

**Design** (agent-os design bar: product-first, the data is the visual, custom SVG glyphs, no emoji, no italics, no generic SaaS cards; harness glyphs are original marks with a color token each, never vendor logos):
- Skill pipeline, one session:
  - `impeccable:frontend-design` (build) → `impeccable:arrange` + `impeccable:typeset` → `impeccable:animate` (edge flow, status pulse; respects `prefers-reduced-motion`) → `impeccable:delight` → `impeccable:polish` → `impeccable:audit` + `impeccable:harden` (a11y, empty/error/overflow states) → `impeccable:adapt` (down to phone width).
  - `shotbox` screenshots each step against the backtest replay (§8), so every design pass is judged on real data.
- Light and dark themes via tokens on `:root`.
- Budget: first paint under 100 ms on localhost, SSE event to paint under 50 ms, and smooth at 200 agents and 50 messages per second (replayed at 50×).

**Security:**
- The server binds `127.0.0.1` only.
- The Host header is checked to block DNS rebinding.
- Every POST requires the per-boot token and a matching Origin.
- The CSP is strict: self only, no inline scripts.

## 8. Testing and backtesting

| layer | what | who writes it |
|---|---|---|
| Fixtures | real hook payloads and transcript slices captured from Claude, Codex, OpenCode and Hermes on this Mac, secrets scrubbed | captured in M0 |
| Acceptance | black-box `assert_cmd` tests of every criterion in §9, the fail-closed branches, and the dashboard HTTP security | **Codex, from this plan, before implementation**; the implementer never edits them, and a change needs spec evidence |
| Unit | route rules, budget arithmetic, hash chain, price math, payload normalisation | implementer |
| Backtest — cost | replay the last 30 days of real Claude and Codex transcripts; per-session cost must match independent ground truth (Claude OTLP `cost_usd` in Beacon; `telemetry/agent-os.db`) within 1% | Codex (harness) |
| Backtest — budget | replay the historical task trees and report which would have stopped at 0.5, 1, 2 and 5 M tokens, and how many tokens were spent after the stop point | Codex |
| Backtest — routing | run `route` and the Jev advisor over the historical dispatches that carry human `worth_it` labels (`jev_routes`); report agreement of each | Codex |
| E2E (local only, needs subscriptions) | the "complete when" scenario with real `claude -p` and `codex exec` | Codex writes the script, the human runs it |
| CI | build, test and backtest-on-fixtures on macOS arm64/x64, Linux x64/arm64 (musl) and Windows x64 | — |

`agent-bus replay <corpus> --speed 50x` drives the dashboard from the backtest corpus. That one mechanism is the demo, the design fixture and the visual-regression input.

## 9. Units, in dependency order, with acceptance criteria

| # | unit | acceptance (turned into tests by Codex) |
|---|---|---|
| M0 | Axon → Cargo workspace (`axon-core` / `axon` / `axon-bus`), moving code with no behaviour change; cargo-dist and a CI matrix including Windows; capture hook fixtures from all four harnesses; confirm that SubagentStart identifies the parent (fallback: the `AGENT_BUS_PARENT` env var) | **Axon's existing `m1..m3` fixture tests pass unchanged after the split**; CI is green on 5 targets; there is ≥ 1 hook fixture per harness per event |
| M1 | store, registry, claims, `hook` for all four harnesses, `install`/`uninstall`/`doctor` | a fixture replay produces the expected tree and statuses; `install` twice is a no-op; `uninstall` restores the backups byte for byte; an UPDATE on `events` fails; `audit --verify` catches an edited row; an empty-inbox hook stays ≤ 10 ms p95 over 500 runs (hyperfine); with no hub, the hook prints nothing, exits 0, and stays within 1 ms (p95) of `/usr/bin/true` on the same host (hyperfine, 500 runs each, unsandboxed) |
| M2 | messages, routing, links and grants, ask/reply, the stop gate | a non-edge send exits 3 and prints the route; replaying the captured Claude `PreToolUse(SendMessage)` fixture for a non-edge target returns `permissionDecision: "deny"` with the route in `permissionDecisionReason`, and an edge target returns no decision; `ask` returns within 1 s of `reply`; a timeout returns the default; a stop denies the next tool call on each harness's reply format; 401-character bodies are rejected |
| M3 | Hermes ingest in `axon-core`; per-tree budgets that join Axon's existing usage rows to bus agents by `session_id`; memory via `sysinfo` | the cost backtest is within 1%; a 50K-token tree is stopped at the next tool call after crossing it; the stale-usage and unreadable-database branches behave as in §6 |
| M4 | `route`, `spawn`, virtual agent, advisors (shadow) | the same input gives the same route; the budget clamp steps down a lane; a virtual node appears as one agent with a panel and a judge; the routing backtest report is produced |
| M5 | `serve` and the dashboard | a narrative fixture per harness renders assistant text, recorded reasoning summaries, the "not recorded" row for empty or encrypted blocks, and collapsed tool chips; with `--content` off, no text leaves the tailer; the security tests pass (403s); a new register reaches the page in < 1 s; the 200-agent replay holds 60 fps (Performance trace); shotbox contact sheet light/dark × desktop/phone approved by the human |
| M6 | release: cargo-dist installers (sh, PowerShell), a Homebrew tap, `cargo install`; agent-os integration | a fresh VM per OS runs install → `agent-bus doctor` green; in agent-os, `link.sh` calls `agent-bus install`, the claim half of `repo-sync` is removed (its `wt-gc`/`wt-census` stay), and CANON's "Repo sync" section points to agent-bus |

## 10. Quality criteria

- **Code:**
  - Rust stable, `clippy -D warnings`, `rustfmt`.
  - Each file ≤ 500 lines. English only. snake_case in SQL and JSON, the same casing on every layer.
  - Parameterized SQL only.
  - No vendor or harness attribution in the UI beyond naming the harness a node runs on.
- **Speed:**
  - Hook ≤ 10 ms p95 with an empty inbox and ≤ 40 ms p95 when delivering.
  - `serve` ≤ 50 MB RSS at 200 agents.
  - Binary ≤ 15 MB.
  - All measured in CI (hyperfine plus the replay).
- **Security:**
  - Trust boundaries are hook stdin, peer message bodies, the HTTP API and outbound HTTP.
  - Hook payloads are validated with serde and rejected on unknown shape (the hook then allows, and logs the rejection).
  - Bodies are framed as untrusted when injected, secret patterns are redacted, and messages never grant permissions.
  - The database is 0600 in a 0700 directory.
  - Outbound calls (prices, Jev, Fusion, OpenCode wake) are off unless configured. The API key comes from an env var and is never stored in the database.

## 11. Execution: fewest turns and tokens

Each unit runs as one fresh session under `goal-loop` with a `/goal` condition, never as one long session (CANON context discipline).

1. **M0, one session.** Scaffold, capture fixtures, write the acceptance-test brief.
2. **Codex, one `codex exec`.** Writes `tests/acceptance/` for M1–M5 from §3–§9 in a single pass. From here on the tests are frozen.
3. **M1 → M4, one session each.** Claude with `coder-playbook` + `ponytail` + `verify`. The goal for each: `cargo test --test acceptance_m<N>` passes and clippy is clean, or stop after 30 turns.
4. **M5, one design session.** Claude with the impeccable pipeline and shotbox, judged against the replay.
5. **Review.** A cross-vendor Codex review of the whole diff, then `post-ship-audit`, with one bounded fix round.
6. **M6, one session.** Release plus the agent-os integration.

That is 8 sessions in total. Each one starts from this plan and the frozen tests rather than re-deriving context.

## 12. Open — the user decides

1. **Name and license:** settled by §0. This ships inside the Axon repo as `axon-bus`, under Axon's MIT license. **The three.js brain stays on the roadmap** (decided 2026-09-29). Findings:
- The Vue/TresJS port was never built (commit `5fe0f21`: "Drops the never-built Vue/TresJS instructions").
- The only recorded reason (`6200a82`) is "no build step, no external assets per §16".
- The server explains why a Vite build could not simply be dropped in. `server.rs` serves exactly one string at `GET /` (`include_str!`), with no static-asset route. A Vite build emits `index.html` plus hashed `/assets/*.js|css`, which would all 404. `rust-embed` and `tower-http/fs` are already declared in `Cargo.toml` but unused.
- Fix: about 30 lines. `#[derive(RustEmbed)]` over `ui/dist`, a `GET /assets/*path` route with a MIME type from the extension, and an `index.html` fallback. It sits behind the same `local_only` guard.
- The "no remote assets" test gate keeps working, because everything stays embedded.
- It lands in M5 only if the brain port is scheduled. The bus views themselves stay single-file vanilla, matching the shipped UI.
2. **Routing: relay by default. Decided 2026-09-29.** Only relay keeps edges enforceable and lets the intermediate stop or redirect a message. `grant --thread --ttl` covers chatty pairs. If direct delivery is ever chosen, the copy goes to the audit log and the dashboard, not into the intermediate's context. What would reverse this: the §8 backtest showing that most historical sends cross between siblings (outside parent ↔ child).
3. **Idle wake for Claude: keep the human dialog as the default. Decided 2026-09-29.** The bus never depends on this setting, because it cannot push into a Claude session (spike Q1). The setting only governs Claude → Claude sends to an idle session, and a peer message is untrusted input. A project whose unattended Claude-to-Claude runs need it can opt in to `crossSessionInbound: accept`; `axon-bus install` never sets it.
4. **Where the repo lives, and the Homebrew tap: decided 2026-09-29.**
   - The repo is the personal account, `github.com/danieltamas/axon`, where Axon already lives.
   - Add a tap at `danieltamas/homebrew-tap`, so users run `brew install danieltamas/tap/axon`. cargo-dist generates and updates its recipe on each release (M6); the only manual step is creating the empty repo once.
   - Intel Homebrew on this Mac would install the x86 build, so on this machine install with `cargo install` or the curl installer. Apple Silicon Homebrew elsewhere gets the native build.
