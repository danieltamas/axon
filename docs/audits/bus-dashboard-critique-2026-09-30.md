# Bus dashboard critique — 2026-09-30

Scope: `crates/axon-bus/ui` project view (topology, agent detail, thread, composer), dark theme at 2000×1250.
Method: impeccable `critique` against `.impeccable.md`, then an independent Codex pass (appended below).
Owner verdict: "sloppy, not premium, AI slop".

## Anti-pattern verdict: fail

- **Terminal cosplay.** Almost every label is monospace, uppercase and letter-spaced: the header, project stats, column heads, role lines, the thread title, the composer segments. Mono is standing in for "technical" rather than marking numbers and identifiers.
- **Neon on near-black.** Emerald dollar figures, an emerald send button and emerald rings on a black-green ground: the default "AI dashboard" palette. Colour decorates money instead of carrying state.
- **Hero-metric strips, twice.** The global header (active/idle/agents/$ bar/unpriced) and the project head (LIVE/CHATTER/MEMORY/TOKENS/SPEND/BUDGET) both open with rows of big-number/small-label stats.
- **Pills for everything.** The attention chips, branch badge, "→ gamerina-web", "All threads" and the segmented controls are all bordered pills of equal weight.

## Priority issues

1. **[P0] Layout spends a quarter of the screen on nothing.** Four regions (tree | empty "Pick an agent" | thread | composer); the detail column is an instruction paragraph whenever no agent is selected. Fix: tree plus one context rail. The rail shows the selected agent, or else the open thread, or else project chatter. The composer lives inside the rail's context.
2. **[P0] The loudest element is the least important.** The full-width green Send button and two stacked segmented controls outweigh the topology. Fix: a compact composer shown only when an agent or thread is in focus; a quiet primary button; kinds in one control that doesn't collide ("REDIRECTQUESTION").
3. **[P1] Cards truncate exactly what the operator reads.** Mission ("Ship M5: dashboard desig…"), model ("CLAUDE-OP…"), "Cross-v…". Meanwhile three right-aligned numbers per card, and "in parent" repeated four times. Fix: two lines per card. Line 1 is name, model and a lowercase model id. Line 2 is the mission, wrapping over up to 2 lines at full width. A single quiet metric line reads `1.2M tok · $2.52 · 64 MB`, and memory is omitted when the agent shares its parent's process.
4. **[P1] Type hierarchy is flat.** Everything is 11–13 px mono caps; agent names barely beat labels. Fix: Space Grotesk in sentence case for names, missions and prose; mono only for numbers, ids and model names, with tabular figures; a real size step (name 15/600, meta 12, labels 11 sentence case, not tracked caps).
5. **[P1] Noise in the project head.** A full absolute `/private/tmp/...` path; alerts at ×9 and ×4 shout in yellow. Fix: show the repo name with the path only on hover. Attention becomes one quiet line in the rail ("2 questions waiting — orch-7f3a, probe-d4") that opens the thread.
6. **[P2] Harness as a 200 px column.** It is a label with a sum. Fix: a lane header row (glyph, name, `3/5 live · $7.59`) above its roots, which returns the width to the tree.
7. **[P2] Rails and arcs are near-invisible** (low-contrast hairlines), so the "data is the visual" promise fails. Fix: stronger connector ink at rest; active paths lit in the harness colour.

## Nielsen scores

| # | Heuristic | Score | Key issue |
|---|---|---|---|
| 1 | Visibility of system status | 3 | live/idle clear; delivery state hidden in tiny meta |
| 2 | Match with the real world | 2 | "you, as coder-a1" and "in parent" are system-speak |
| 3 | User control and freedom | 2 | no obvious way out of a thread besides a floating ghost pill |
| 4 | Consistency and standards | 2 | same pill shape for badges, filters, buttons, alerts |
| 5 | Error prevention | 3 | stop is never the default; stop styled as danger |
| 6 | Recognition over recall | 2 | truncated models and missions force hover/recall |
| 7 | Flexibility and efficiency | 2 | no keyboard navigation of the tree |
| 8 | Aesthetic and minimalist design | 1 | duplicate stat strips, empty column, caps everywhere |
| 9 | Error recovery | 3 | refusals shown inline with the server's reason |
| 10 | Help and documentation | 2 | instruction paragraphs instead of teaching empty states |
| | **Total** | **22/40** | acceptable — significant improvements needed |

## Codex pass (independent, gpt-5 via codex exec, read-only)

**1. Where Claude’s critique falls short**

- Emerald on green-black is Axon’s identity. The problem is indiscriminate emphasis: money, activity, budgets and actions compete for the same colour.
- “Two questions waiting” misreads the screenshot: two grouped relationships contain **13 unanswered messages**. These are agent-to-agent obligations, not necessarily questions addressed to the operator.
- Agent IDs belong in monospace. Making identifiers look like prose will not fix hierarchy. Give missions more space and roles more prominence.
- The rings encode **budget consumption**, not activity. The critique misses this ambiguity, ambiguous monetary totals, and a “project budget” that actually reports the highest individual utilisation.
- Two panes is the right direction, but needs explicit selection, navigation and draft-preservation rules.

**2. Redesign specification**

**Shared shell.** Use a 56px header: brand, breadcrumb, connection state, theme control. Remove global metrics from project view. Use 24px desktop gutters, 16px mobile gutters, 8px spacing increments, 6px control radii. No gradients or glow.

**Overview, ≥1280px.** One full-width project list beneath a scoped summary and attention strip. Five columns: Project, Current work, Active agents, Priced usage, Attention; widths `2fr 3fr 1fr 1.5fr 1.5fr`, gaps 16px. Project cell: name, shortened path, harness glyphs. Current work: root missions, each wrapping to two lines. Replace miniature topology and resource columns with project-level disclosure. Preserve alphabetical order during updates. Clicking project name opens its tree; clicking attention opens the relevant question or fault.

**Overview, 390px.** Stack each project into three rows: name and attention count; current work; active agents and priced usage. Put harness names beneath the project name. Resource disclosure expands in place. No horizontal scrolling or persistent chatter rail.

**Project, ≥1280px.** Grid: `minmax(0,1fr) 400px`, 24px gap. Left: project title, scoped totals, attention strip, topology. Each harness gets a section heading. Families use a 300px root column and remaining-width children column, separated by 32px. Deeper children indent 20px. Roots remain expanded; completed subtrees collapse with their count.

Right: one context pane. Default is project threads. Selecting an agent opens Narrative, with Threads and Metrics tabs. Selecting a thread opens its chronological conversation; Back restores the list. Never reserve an empty narrative column. Metrics contains memory, pricing breakdown, burn rate and budgets labelled by owner.

**Project, 390px.** Single pane with Agents / Activity / Attention tabs beneath the header. Tree becomes an outline, indented 16px per level, capped at 32px; deeper rows explicitly name their parent. Agent/thread selection opens a full-width detail view with Back. Preserve scroll and selection when returning.

**Agent anatomy**, identical at both widths:

1. Harness glyph, role, exact ID; right-aligned status word.
2. Mission, maximum two lines; full text in detail.
3. Full model identifier, wrapping.
4. Tokens · priced usage · owned RSS · budget percentage when configured.
5. Conditional branch and cross-repo destination.

Use 12px padding. Keep budget rings only on budget owners, paired with percentage text. Drop repeated “in parent”, uppercase model transformations and bordered metadata badges. Explain shared memory in Metrics.

**Typography.** Space Grotesk: titles 22px/600; roles 14px/600; missions/body 14px/400; controls/labels 12px/500. JetBrains Mono: IDs, models and metrics 12px/400, tabular numerals. Sentence case throughout except AXON; no tracking. Line-height 1.4. Bundle fonts locally.

**Colour.**

| Role | Dark | Light |
|---|---|---|
| Ground / surface | `#05080A` / `#0F1814` | `#F1F4F0` / `#FFFFFF` |
| Text / secondary | `#D3E3D9` / `#93A398` | `#17251D` / `#4A5A51` |
| Signal | `#34D399` | `#0B7D58` |
| Attention | `#F5C451` | `#8F6204` |
| Failure | `#FF6B72` | `#C02A36` |

Keep prices neutral. Harness colours identify glyphs; emerald marks activity, selection and message movement. Amber marks pending questions/budget warnings; red marks failures/stops. Always pair colour with text. Closed agents retain readable contrast. Highlight selected routes; respect reduced motion.

**Composer.** Appears after Answer, Redirect, Question or Sync is chosen. Show recipient and “Sent as [agent]”; answering locks the exact question and route. One action dropdown, recipient dropdown only when necessary, three-line textarea, 400-character counter, compact submit button. Enter inserts newline; Cmd/Ctrl+Enter sends. Stop is separate and confirms target plus affected subtree. Preserve drafts across navigation and snapshots. Distinguish sent, delivered, answered and failed. Mobile controls have 44px targets; composer stays above the keyboard.

**Attention and empties.** Persistent count opens grouped items: stopped/orphaned first, unanswered questions oldest-first, budget warnings last. Each exposes count, age, owner and direct action. Never silently truncate. Unknown telemetry displays “—”; partial prices say “N unpriced”. Distinguish Connecting, No agents, No messages and Content capture off. Empty attention reads “Nothing needs attention.” Keyboard navigation preserves visible focus.

**3. References to borrow**

- [Linear Inbox](https://linear.app/docs/inbox): separate actionable updates from routine activity; keyboard-driven selection.
- [Datadog Trace View](https://docs.datadoghq.com/tracing/trace_explorer/trace_view/): selected-node inspection with preserved ancestry and reversible subtree focus.
- [Sentry Issue Details](https://sentry.io/changelog/new-issue-details-ui-now-available/): grouped workflow actions and progressive disclosure of secondary metadata.

**4. Structural obstacles**

- [app.js](crates/axon-bus/ui/app.js:14) maintains independent agent/thread selections and two forms. Replace these with explicit context navigation and drafts keyed by recipient/question.
- [send.js](crates/axon-bus/ui/send.js:17) assumes agent impersonation; root controls lack route resolution. Human attribution requires an API change.
- [overview.js](crates/axon-bus/ui/overview.js:20) collapses attention into strings, losing actionable IDs; monetary snapshots lack billing basis.
- [board.js](crates/axon-bus/ui/board.js:11) and `chatter.js` couple geometry to horizontal cards. Update anchors with layout; retain keyed nodes.
- Fonts are unbundled; Rust’s asset manifest needs font support. Preserve CSP, CSSOM updates and existing performance budgets.