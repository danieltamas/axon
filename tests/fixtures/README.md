# Test fixtures (redacted, real-shaped)

Derived from Claude Code logs **verified on a real machine** (DESIGN.md Appendix A). Content is redacted; structure + the numbers that matter are faithful. These are the M1 acceptance gates.

## `claude_main.jsonl` — the collapse / no-double-count gate
- 2 lines, **both `message.id == "msg_FIXTURE01"`**, each repeating the **identical** `usage` (`output_tokens: 10498`).
- **Expected:** exactly **ONE** Event (not two). `tokens_out == 10498` (**not** 20996). `content` = union of the two lines → an `Edit` (`toolu_A`) **and** an `Agent` spawn (`toolu_B`, `subagent_type: "security"`).
- `timestamp` parses ISO-8601 → epoch ms.
- LOC: the `Edit` turns `"a"` into `"a\nb\nc"` → counted once, only because the (implied) tool_result succeeds.
- Agent for this Event = `"main"` (`is_subagent == false`).

## `claude_subagent.meta.json` + `claude_subagent.jsonl` — the attribution gate (M1 GATE)
- meta `toolUseId == "toolu_B"` → **joins to the parent `Agent` spawn above** → this sub-agent IS the `security` agent (`agentType`).
- `claude_subagent.jsonl`: `isSidechain: true`, 2 lines sharing `message.id "msg_SUB01"` → **ONE** Event, `tokens_out == 850`, `model == "claude-sonnet-4-6"`, `agent == "security"`, `is_subagent == true`.
- **Expected attribution:** the 850 output tokens (+5000 input) go to agent `"security"`, **not** `"main"`.
- **Session totals:** main turn (10498 out, `"main"`) **+** sub-agent (850 out, `"security"`). The two are disjoint — the parent `Agent` tool_use line carries the parent's tokens only; the child's tokens live here. **No double-count between parent and child.**

## `codex.jsonl` — Codex cumulative-delta gate (M2)
Real-shaped, redacted. Verified against `~/.codex/sessions`: lines are `{timestamp(ISO), type,
payload}`; usage is **session-cumulative** in `event_msg`/`token_count`
`payload.info.total_token_usage`, with no `turn_id` on it; the turn key is
`turn_context.payload.turn_id` (+ `model`); LOC comes from `patch_apply_end`.
- Two `turn_context` boundaries → **two** Events (per-turn cumulative delta, NOT raw totals).
- turn-1: input 1000 − cached 200 → `tokens_in 800`, `cache_read 200`, `tokens_out 500`,
  `loc_added 2` (unified diff `+b +c`), model `gpt-5.5`, agent `codex`.
- turn-2: cumulative 4000/700/1200 → delta → `tokens_in 2500`, `cache_read 500`, `tokens_out 700`.

## `opencode_message.json` — OpenCode reported-cost gate (M2)
One assistant `message.data` JSON from the real `opencode.db` `message` table (redacted).
OpenCode stores its own computed `cost` and epoch-ms `time` (NOT ISO).
- → ONE Event: `tokens_in 700`, `tokens_out 200` (output 150 + reasoning 50), `cache_read 40`,
  agent `build`, model `gpt-5.5`, harness `opencode`.
- **Cost uses OpenCode's reported `cost`** (0.0123 USD × fx), `unpriced == false` — Axon trusts
  the harness's own number for its many gateway models. Ollama turns report `cost 0` → free.
