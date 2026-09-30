# Codex review — observed sessions and transcript narrative (2026-09-30)

Reviewer: Codex (read-only sandbox), cross-vendor. Scope: commits e6b9928, 1779610.

Eight findings. Lines refer to `1779610`; R2, R3 and R5 involve pre-existing code paths.

- **R1 · P1 · [cli_guard.rs:106](/Users/danime/Sites/axon/crates/axon-bus/src/cli_guard.rs:106)** — `serve --no-content --content` passes the guard, but clap applies the later flag and enables capture. Reproduced. **Solved when:** guard decisions match effective CLI options, including conflicting flags.

- **R2 · P1 · [snapshot.rs:131](/Users/danime/Sites/axon/crates/axon-bus/src/snapshot.rs:131)** — A `--no-content` server returns transcript text after another server renews the shared capture lease. The server’s own privacy option never reaches snapshot filtering. Reproduced over HTTP. **Solved when:** that server’s snapshot and SSE remain structure-only regardless of other servers.

- **R3 · P1 · [hook.rs:117](/Users/danime/Sites/axon/crates/axon-bus/src/hook.rs:117)** — Holding a database write lock makes the fallback call the gate with `Value::Null`, discarding the shell command. Plain `axon-bus serve`, normally denied, becomes allowed. Reproduced. **Solved when:** command restrictions remain enforced during registry-write failures.

- **R4 · P1 · [snapshot.rs:184](/Users/danime/Sites/axon/crates/axon-bus/src/snapshot.rs:184)** — Newly exported bus messages bypass capture filtering and redaction. With `content:false`, an operator redirect containing a private prompt and synthetic `sk-…` credential returned both verbatim. **Solved when:** structure-only snapshots omit message bodies and displayed bodies undergo redaction.

- **R5 · P1 · [transcript.rs:345](/Users/danime/Sites/axon/crates/axon-bus/src/transcript.rs:345)** — The redactor reused by `tail.rs` misses `{"password":"correct-horse-battery"}` and deliberately preserves `password=12345678`. Both survived actual storage. **Solved when:** quoted JSON credentials and numeric secrets are masked without masking ordinary token counts.

- **R6 · P2 · [observed.rs:130](/Users/danime/Sites/axon/crates/axon-bus/src/observed.rs:130)** — Two processes sharing a directory exchange transcripts, usage and missions whenever their latest-turn ordering changes. Matching is recalculated greedily without identity evidence. Reproduced. **Solved when:** attribution follows verified session identity; ambiguous matches remain unknown.

- **R7 · P2 · [context.js:200](/Users/danime/Sites/axon/crates/axon-bus/ui/context.js:200)** — Appending another tool to the final collapsed run changes neither row count nor its timestamp. The rendering cache therefore leaves the panel stale. Reproduced through the actual module. **Solved when:** appended calls update displayed counts and details immediately.

- **R8 · P2 · [observed.rs:247](/Users/danime/Sites/axon/crates/axon-bus/src/observed.rs:247)** — Future timestamps pass the query and clamp into the current-hour bucket. A fixture four hours ahead increased today’s current-hour count. **Solved when:** future events cannot inflate current activity.

Checked clean:

- DB IDs reject traversal syntax before `locate()`; this does not establish symlink containment.
- Tail capture-off transitions remove cached prompts/text; stored narrative queries suppress text/details.
- UI text uses `textContent`; no HTML-injection or evaluation sinks found.
- Unchanged tails use cached parsing. Optimized **70 × 512 KiB** probe: **0.22 ms unchanged, 1.29 s changed**. This measures the tail phase, not the entire snapshot loop.
