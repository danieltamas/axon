# Handoff: BUS-PLAN §3c B–E (tasks) + "awaiting an answer" fix → 0.4.6

Status 2026-10-03: done, shipped in v0.4.6 (194dd33): `acceptance_tasks` green (17 tests), the "awaiting an answer" fix included, CI tagged v0.4.6. Original goal: `cargo nextest run -p axon-bus -E 'binary(acceptance_tasks)'` green, the UI fix below, then `chore: release 0.4.6`.

## Decisions already taken (don't re-derive)

- **Placement:** prompt extraction in `axon-core` ingest; assignment, API and CLI in a new `crates/axon-bus/src/tasks/` (mod.rs assign, api.rs beside handled/api.rs, cli.rs). axon-core cannot see the bus tables (ledger, agents, messages). Call the assignment from root `src/main.rs` scan right after `record_scan`. Note: `src/main.rs` is 510 lines, so move `open_in_browser`/scan helpers out to get it under 500.
- **Assignment by timestamps, immutable:** `INSERT OR IGNORE INTO task_turns`. Order:
  1. Own open take: `take_ts <= turn.ts < done_ts`; the latest take wins.
  2. Nearest ancestor with an open take (bus `parent` chain).
  3. Delegation: a handoff/question from the taker to this agent at M, until this agent sends answer/ack **in that thread** at E (M <= ts < E).
  4. Otherwise a request: the latest prompt in the same session with `prompt.ts <= turn.ts`. Subagent turns carry the parent's sessionId, so they fold in naturally.
- **Turn → bus agent:** main turns map to the root agent whose session = `session_id`; subagent turns map to the agent id = transcript `agentId`. `RawTurn.agent_id` already exists but `Event` drops it. Add `agent_id` (and an MCP `tools` JSON) to `Event` and to `usage_events` columns, via `migrate_schema` in `crates/axon-core/src/store.rs`.
- **Prompts:**
  - Claude: in `collapse` (`ingest/claude.rs`), a `type:"user"` line is currently only scanned for failed tool results. Emit a prompt when its content is a string, or an array without `tool_result`.
  - Codex: an `event_msg` with `payload.type == "user_message"`.
  - Return prompts beside the turns; `Source::parse` returns turns only today, so extend it. Store them in a `task_prompts`/requests table.
- **Ids:** request = `blake3(session_id, prompt_ts)`; declared = derived from the ledger take row (key + taken_at). Declared rows are upserted from the ledger on every scan, so `closed_at` follows `done`.
- **Request:** `opened_at` = prompt ts, `closed_at` = the session's next prompt ts. Name: `request at HH:MM` (UTC, chrono format) when capture is off; when on, the first 80 chars after `redact.rs`.
- **Cost:**
  - `measured` = Σ `cost_eur` of in-range turns;
  - `credits` = Σ `cost_credits`;
  - `estimated` = 0 (no compute-rate setting yet; say so);
  - priced `[tools]` calls are folded into the turn's `cost_eur` at ingest, so task totals == `/api/summary` to the cent;
  - unpriced MCP names (`mcp__*` tool_use names not in `[tools]`) go to `tools_unpriced`;
  - `retries_cost` = 0 (errored turns aren't tracked yet; say so).
- **human_wait_ms:** for a request, `closed_at` − its last turn ts; for a declared task, the sum of gaps before each prompt in the taker's session inside the window.
- **Range:** list tasks with ≥1 turn in range, plus open declared tasks with no turns for `all`; sum only in-range turns; newest `opened_at` first; at most 200 rows. Owner session required (401). CLI: `axon bus tasks [--repo] [--json]`, the same rows.
- **UI (§3c D):** extend `ui/handled.js` "Handled work" → "Tasks", plus a top-tasks card in the usage view.

## Also in 0.4.6

- The dashboard shows a question as "awaiting an answer" forever when the recipient session has closed. Show it as unanswered/closed instead.

## Verify

Run only the affected binaries locally (`acceptance_tasks`, `acceptance_turn_project`, the fixtures). CI is the full gate. Update DESIGN.md plus the local LOG.md/ARCHITECTURE.md.
