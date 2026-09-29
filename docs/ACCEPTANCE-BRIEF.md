# Acceptance-test brief for axon-bus (for Codex)

You write the acceptance tests for milestones M1–M5 of `docs/BUS-PLAN.md`, in one pass, before any implementation exists. Source of truth: BUS-PLAN §00, §2, §3, §6, §7 (security) and the §9 table, including the "M0 result" block under it. This brief only resolves naming and layout, and records facts M0 measured. Where it and the plan disagree, the plan wins; say so in a comment.

## Rules
- **Tests are frozen once written.** The implementer never edits them. A change needs spec evidence (a plan section or a captured payload) showing the test is wrong.
- **Black-box only.** Run the `axon-bus` binary (`assert_cmd`) and read its stdout, stderr, exit code and the SQLite file it writes. Never import `axon-bus` internals. `axon-core` types may be used only to build usage rows.
- **Hermetic.** Every test sets `HOME` and `XDG_DATA_HOME` to its own `tempfile::TempDir`. The database is `$XDG_DATA_HOME/axon/axon.db`, shared with `axon` (BUS-PLAN §0). No test may read or write a real harness config, reach the network, or call a model.
- Dev-dependencies you may add to `crates/axon-bus/Cargo.toml`: `assert_cmd`, `predicates`, `tempfile`, `rusqlite` (already a workspace dependency), `serde_json`. Nothing else without a written reason.

## Layout
- `crates/axon-bus/tests/acceptance_m1.rs` … `acceptance_m5.rs`, one per milestone. They run with `cargo test -p axon-bus --test acceptance_m<N>`, which BUS-PLAN §11 names as each milestone's goal.
- Shared helpers go in `crates/axon-bus/tests/common/mod.rs`: temp env, running a hook with a fixture, opening the DB.
- Latency gates (M1: empty inbox ≤ 10 ms p95; no hub within 1 ms p95 of `/usr/bin/true`) are **not** cargo tests. Write them as `crates/axon-bus/benches/hook-latency.sh`, using `hyperfine -N --warmup 20 --runs 500 --export-json`, and have it exit non-zero when a gate fails.

## Names (plan text → this repo)
| plan says | use |
|---|---|
| `agent-bus` (binary and commands) | `axon-bus` |
| `AGENT_BUS_PARENT` | `AXON_BUS_PARENT` (renamed with the binary, §0) |
| `src/*.rs`, `tests/acceptance/` | `crates/axon-bus/src/*.rs`, the files above |
| `tests/fixtures/<harness>/` | `tests/fixtures/hooks/<harness>/<event>[.child].json` at the workspace root (see its README) |
| `serve` port | `127.0.0.1:7433` (§1); `axon` keeps 7777 |

Hooks are invoked as `axon-bus hook <harness> <event>`, with the payload on stdin. `<harness>` is one of `claude`, `codex`, `opencode` or `hermes`; `<event>` is the harness's own event name, exactly as in the fixture file names.

## Facts M0 measured (encode these; do not re-derive them)
- **Registration is lazy.** Claude `SessionStart` does not fire under `claude -p`. The first hook of any kind for an unknown `session_id` must register the agent. Test it by replaying `claude/PreToolUse.json` alone and expecting one registered agent.
- **Parent linkage per harness:** see the §9 "M0 result" table. The expected tree from a replay:
  - Claude and Codex: the child `agent_id` sits under the session root.
  - Hermes: `child_session_id` sits under the parent `session_id`.
  - Codex children nested deeper than one level need `AXON_BUS_PARENT`.
- **Reply formats for deny and inject**, per harness:
  - Claude and Codex, `PreToolUse` deny: `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":…}}`. Spike Q2 verified this for Claude; the Codex wire types in 0.153.4 are the same.
  - Claude and Codex, `PostToolUse` inject: `hookSpecificOutput.additionalContext`.
  - Hermes: `pre_tool_call` deny is `{"decision":"block","reason":…}`, and `pre_llm_call` inject is `{"context":…}` (hermes-agent `agent/shell_hooks.py` wire protocol).
  - OpenCode (a plugin shim calls the binary): `{"decision":"deny","reason":…}` on stdout, which the shim turns into a throw. This is a brief decision, because the plan leaves it open.
- **No hub:** with no `axon.db`, every hook exits 0 and prints nothing (§00).
- **Claude `SendMessage` policing** (§3, M2): replaying a `PreToolUse` payload with `tool_name: "SendMessage"` and a non-edge `tool_input.to` must deny, with the route in the reason. An edge target must print no decision. Build the payload from `claude/PreToolUse.json`, changing only `tool_name` and `tool_input`.

## What to cover (one test per bullet at minimum)
- **M1:**
  - Fixture replay of all four harnesses gives the expected tree and statuses (§2 "Status is taken from hooks").
  - `install` twice is a no-op.
  - `uninstall` restores the backups byte for byte (against a temp HOME seeded with copies of the real config shapes).
  - An `UPDATE` or `DELETE` on `events` fails.
  - `audit --verify` detects a row edited through a trigger-bypassing copy of the DB.
- **M2:**
  - A non-edge send exits 3 and prints the route.
  - `ask --wait` returns within 1 s of `reply`; a timeout returns `--default`.
  - A `stop` denies the next tool call, in each harness's reply format.
  - A 401-character body is rejected; 400 characters is accepted.
  - `link` / `accept` / `grant --ttl` open edges, and an expired grant closes its edge.
  - The Claude `SendMessage` deny and allow cases above.
- **M3:**
  - A 50K-token tree budget stops the next tool call after it is crossed.
  - Warning at 80%.
  - Every §6 fail-closed branch:
    - usage older than 120 s on an active budgeted tree warns once, then denies at 90% of the last known total;
    - an unreadable DB with a doorbell stop denies;
    - an unreadable DB with no doorbell allows.
  - Tree cost equals the sum of `usage` rows × prices from `axon-core`.
  - An unknown model id is *unpriced*, never $0, and a budgeted tree with unpriced usage falls back to its token ceiling (§9b). `claude/PostToolUse.json` is a real case: its subagent resolved to `claude-sonnet-5-5`, which the bundled `pricing.toml` does not price.
- **M4:**
  - The same `route` input gives byte-identical output.
  - The budget clamp steps down one lane and says so. Its typical-cost median is keyed by **role × model × effort**, never role alone (§4): seed history where the same role has an expensive old model and a cheaper new one, and check that the new model's median decides.
  - `spawn --virtual` registers one addressable node with panel and judge children.
  - Advisor output is logged as a shadow and never changes the rule's answer.
- **M5 (HTTP against `serve`):**
  - A non-loopback Host gets 403.
  - A cross-origin POST, or a POST without the per-boot token, gets 403.
  - The CSP header is present and has no `unsafe-inline`.
  - `/api/snapshot` groups repo → harness → root agent → subagents (§7): a worktree agent sits under its main repo, a child working in another repo stays under its parent with a repo badge, and an agent outside git sits under "no repo". Build the repos with `git init` and `git worktree add` in the temp dir.
  - A new registration reaches `/api/stream` within 1 s.
  - With `--content` off, no assistant text appears in `/api/snapshot`.
  - Narrative fixtures render text, reasoning summaries, the "not recorded" row, Claude progress-update `thinking` blocks as a distinct **progress** row (never reasoning, §7), and collapsed tool chips, as JSON in the snapshot. Visual review is the human's job.

## Out of scope for these tests
- The §8 backtests (cost vs. Beacon ground truth, budget replay, routing agreement), which are a separate harness.
- The M5 60 fps trace and the shotbox sheet.
- M6 release and VM installs.
- The real-harness end-to-end run of §1.
