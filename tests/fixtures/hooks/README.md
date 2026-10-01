# Hook fixtures (BUS-PLAN §9, M0)

Real hook payloads, captured 2026-09-29 on macOS arm64, one file per harness per event. Each file is exactly what a hook receives: stdin JSON for Claude, Codex and Hermes, and the plugin hook arguments for OpenCode. Paths are rewritten to `/tmp/axon-fixture` and `/Users/user`. The export fails on anything that looks like a secret, token, email or local username.

`<Event>.child.json` is the same event fired inside a subagent.

| harness | version | how it was captured | events |
|---|---|---|---|
| Claude Code | 2.1.284 | `claude -p --settings <hooks>` (Haiku); `SessionStart` from a short interactive session, because **`SessionStart` does not fire under `-p`** | SessionStart, UserPromptSubmit, PreToolUse(+child), PostToolUse(+child), SubagentStart, SubagentStop, Stop |
| Codex CLI | 0.153.4 | `codex exec -c 'hooks.<Event>=[…]'` (session-flag layer; `~/.codex` untouched) | SessionStart, UserPromptSubmit, PreToolUse(+child), PostToolUse(+child), SubagentStart, SubagentStop, Stop |
| OpenCode | 1.18.31 | project plugin `.opencode/plugin/capture.js`, `opencode run` (local gemma4:26b) | session.created, tool.execute.before, tool.execute.after |
| Hermes | hermes-agent `c2ca3f01a` | project plugin in a scratch `HERMES_HOME` (`plugins.enabled`), serialised with Hermes's own shell-hook `_serialize_payload`; `hermes -z` against local Ollama gemma4:26b | on_session_start, pre_llm_call, pre_tool_call, post_tool_call, subagent_start, subagent_stop |

**Missing:** none of the four v1–v3 harnesses in M0 scope.

**Not captured:**
- OpenCode child sessions: the local model ended before the `task` subagent ran.
- Gemini CLI: v3, and it has no credential on this Mac (BUS-PLAN §00).

**Parent linkage (recorded in BUS-PLAN §9, M0):**
- Claude and Codex: `SubagentStart` carries the parent's `session_id` plus a new `agent_id`, and every child tool call carries both.
- Hermes: `subagent_start` names the parent and child explicitly.
- OpenCode: `parentID` is in its session schema but was not observed.
