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

## `codex.jsonl` / `opencode.log`
TODO — capture real redacted samples during M2 (verify §6.2 / §6.3 against real files first; do not assume).
