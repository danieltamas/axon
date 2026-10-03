# Axon log

What changed and why, newest first. One entry per release or decision. The detail lives in
the specs ([docs/BUS-PLAN.md](./docs/BUS-PLAN.md), [docs/P2P-SPEC.md](./docs/P2P-SPEC.md)) and
the structure in [ARCHITECTURE.md](./ARCHITECTURE.md).

## 0.4.4 (2026-10-03)

- **Same-repo auto-link.** Live root sessions of any harness in the same repository can now
  message each other without a `link`.
  - *Why:* an agent told the user it could see the other session in the repo but the bus
    refused its messages. The user said they should not be in that flow.
  - *Guardrails:* only roots (not subagents) that are active or idle, of a known harness,
    whose cwd resolves to the same repository. A session in another repository still needs
    a mutual link.
  - *Switch:* Settings → Agents ("Link sessions in the same repository"), on by default.
    It is exposed as `PUT /api/settings/bus {auto_link}`. BUS-PLAN §3.
- **Handled ledger.** `axon bus take | done | drop | handled` record work items that are not
  files (a lead, an issue, a URL) once per repository.
  - *Why:* a user asked whether Axon knows what work is already handled, or whether that
    stays the operator's job. Claims covered files only.
  - *Where it shows:* the SessionStart introduction and the guide teach it, and the project
    view lists it. BUS-PLAN §3b.
- **Ledger over federation.** A share carries the ledger both ways its flags allow, with a
  deterministic winner when two machines claim the same item. P2P-SPEC §7b.
- **Connections view.** Federation moved out of Settings into a top-level view with:
  - a link map: each wire's state, messages and KB each way, queued messages, and agents
    per harness on both ends;
  - pairing up front;
  - one card per peer.

  Byte counts moved from `counters` to a new `traffic` object in the peer health.
- **Federation fixes:** one share per repository per peer, and remote sessions that go away
  stay addressable for a message lifetime, so messages sent overnight queue instead of
  failing.

## 0.4.3 (2026-10-03)

- On Windows the home folder falls back to `USERPROFILE`, so the hub, hooks and logs stop
  landing relative to each process's folder.
- Settings keeps its top bar fixed, dropdowns survive live updates, and selects draw their
  own chevron.
- Test servers wait within each test's declared budget, because macOS checks a freshly
  linked binary on first launch.

## 0.4.0 to 0.4.2 (2026-10-02)

- **Federation shipped (0.4.0):**
  - pairing (invite, join, pair-code confirmation);
  - shares and discovery;
  - messages both ways with bounded, quoted framing;
  - pausing and removing a peer;
  - per-peer rate limits on every frame kind;
  - health on `/api/stream`.

  Three Codex review rounds and fix rounds followed. The accepted residuals are listed in
  P2P-SPEC §12.
- The owner session cookie needs its token on every API route, and signing out ends open
  streams.
- 0.4.1 and 0.4.2 were Windows fixes (lock pid file, key ACL, fixture paths). A subagent
  now shows its type, task, brief and handback.
- The README became a demo page. CLI and federation detail moved to `docs/USAGE.md` and
  `docs/FED-MANUAL.md`.

## 0.3.0 and 0.3.1 (2026-10-01)

- **One app:** the control plane (`axon bus`) and the usage dashboard became one binary,
  one database and one page. BUS-PLAN §0b.
- The dashboard is plain ES modules embedded in `axon-bus`, replacing the Vue/TresJS plan
  in DESIGN.md §10.

## 0.1.0 to 0.2.1 (2026-06-15 to 2026-06-20)

- Local, cross-harness usage observability: transcript ingest (Claude including
  subagents, Codex, OpenCode), cost by model, the brain view and the CLI summary.
  DESIGN.md.
