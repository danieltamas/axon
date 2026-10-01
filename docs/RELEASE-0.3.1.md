# Release: v0.3.1

A fix release on 0.3.0, found by running the bus on a real machine for a day.

## Fixes

- **Sessions that die are closed.** A session whose harness exited without saying so (a
  crash, a killed terminal, a hook from an older version) stayed "working" forever, with its
  subagents. While `axon` runs, each process sample closes a session whose process is gone,
  or whose pid now belongs to a newer process, together with every subagent under it, and
  releases their claims. When the process table cannot be read (a sandbox), nothing is
  closed.
  A session that registered or resumed after a sample is never judged by it.
- **A reopened session forgets its end time.** A resumed session no longer carries the end
  time of its previous run.
- **Message arcs fade.** The arcs between agents on the topology are messages in flight:
  they now fade out within 90 seconds instead of staying drawn at a quarter strength
  forever. The arcs of the thread you have open stay lit.
- **Structured replies read as fields.** An agent message, or a line an agent said, that is
  one JSON object (a verdict, a status) shows as labelled fields in the detail pane and as
  `Label: value` pairs on one line in the topology and thread lists.
- **Agent text reads as markdown.** What an agent said, and the messages agents send, render
  their code blocks, headings, lists, quotes, tables, inline code, bold and links instead of
  showing the raw markup. It is built as plain DOM nodes, so agent text can never inject
  HTML into the dashboard.

## Agents know more about Axon

- The introduction every agent gets now says what Axon is, and adds the working rules:
  answer questions you receive, message only when it changes someone's work, claim paths
  before editing a shared checkout, stop and report on a stop or budget denial.
- **`axon bus guide`** prints the full agent playbook: who you can reach and how, message
  kinds, threads and refs, claims, stops and budgets, and how bus commands run through the
  hook. The introduction points to it.

## Known limitations

- **Closing exited sessions needs a pid.** On Windows the hooks record none, so a session
  there that exits without a SessionEnd stays open, as in 0.3.0.
- **A process the system will not describe counts as exited.** If the process table lists
  this server but hides one harness process (another user's, a hardened process), that
  session is closed. Harness sessions run as the same user as `axon`, so this is not
  expected.

## Upgrading

Install over 0.3.0 and restart `axon`. No database change. Hooks keep pointing at the same
`axon` path.
