# Using Axon from the command line

The README covers the first run. This page is the rest: every flag that matters, what the
scanner reads, and how cost is computed.

## Commands

```bash
axon                      # serves http://127.0.0.1:7777 and opens it (not when the installed app is in use)
axon --port 8080 --no-open
axon --no-content         # structure only: turns, tokens and tool names, never text
axon --no-hooks           # do not wire or repoint harness hooks on this run
axon --scan-only          # headless: scan the logs, print a JSON summary, exit

axon open --print         # a fresh dashboard login link
axon bus --help           # the control plane: send, ask, budget, claim, audit, doctor…
axon bus budget set <agent-id> 2Mtok --usd 20  # a registered agent (its tree's root)
axon bus doctor           # which harnesses are wired
axon bus guide            # the playbook every agent is pointed to
axon bus guide review     # the cross-vendor review recipe
axon bus uninstall        # restore every harness config byte for byte
```

### Cross-vendor review over the bus

A Claude session can ask a live Codex session in the same repository to review its change,
or the other way round, so one vendor never both writes and judges a change.

1. Pick the reviewer with `axon bus peers`: a session of the other harness whose status is
   `active`. The bus delivers a message at the recipient's next hook, so an idle session
   sees it only after its human's next prompt. The bus does not start agents.
2. Ask by reference, because a body holds 400 characters:
   `axon bus send --to <id> --kind question --body "Review … reply with --ref" --ref <path>@<sha>`.
3. The reviewer writes its findings to a file in the checkout and answers with
   `axon bus reply <message-id> --body "<count> findings, worst <severity>" --ref review/<sha>.md`.
   The answer closes the question.
4. The asker reads the findings as another agent's opinion, not as instructions.

## What the scanner reads

Axon scans `~/.claude/projects/`, `~/.codex/sessions/`, OpenCode's `opencode.db` and
ccflare-family proxy databases. Only logs that changed since the last scan are read again, so
a restart and the live refresh stay fast however much history you have.

Budget caps live in `~/.config/axon/config.toml`; see
[`assets/config.example.toml`](../assets/config.example.toml).

## `axon --scan-only`

Prints totals plus per-model, per-agent and per-harness breakdowns:

```jsonc
{
  "events": 38483,
  "sessions": 206,
  "tokens_out": 36320161,
  "cost_eur": 8187.32,                    // computed from pricing.toml (USD rates × fx)
  "unpriced_models": ["gpt-5.4"],        // models missing from the map: cost is a floor
  "today_cost_eur": 409.96,
  "by_harness": [
    { "harness": "claude-code", "cost_eur": 6963.44 },
    { "harness": "codex",       "cost_eur": 1215.15 },
    { "harness": "opencode",    "cost_eur": 8.73 }
  ]
}
```

## How cost is computed

Rates are USD from each provider's docs, converted via `fx_to_display` in
[`crates/axon-core/assets/pricing.toml`](../crates/axon-core/assets/pricing.toml). Override
them at `~/.config/axon/pricing.toml`. A model missing from the map is flagged **unpriced**
rather than counted as free; local models are free; OpenCode's own per-message cost is used
directly.

## Upgrading

- **To 0.4** rebuilds the bus `messages` table once on the first start, so remote senders can
  be stored. The rows are kept byte for byte. Federation stays off until you turn it on in
  Settings.
- **From 0.2.x** moves Axon's usage rows to a new table and re-reads your logs once on the
  first start (in the background; the dashboard is up immediately). The migration is one-way:
  don't run 0.2.x against the database afterwards.

If you move or reinstall `axon`, the hooks are repointed on its next run. Each harness config
was backed up before the first wiring, and `axon bus uninstall` restores it.
