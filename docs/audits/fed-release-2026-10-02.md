# Release test: `job/fed` (federation, owner login, Settings)

- Date: 2026-10-02. Tester: release-tester agent, read-only on src and tests. Host: macOS arm64, rustc 1.96.0. Branch HEAD `988ee52`-based worktree `axon-wt-fed`, tree clean.
- Scope: CI and cross-platform readiness, release build, `scripts/fed-e2e.sh` x2, fresh-user README walkthrough (isolated HOME/XDG, port 7791), FED-MANUAL review.

## Stance

**1 P0 - 2 P1 - 7 P2 - 5 P3.** Not releasable until REL-1 is fixed. Bare `axon`, the first command in the README, panics at startup. Everything else in the walkthrough only worked through `axon bus serve`.

Not confirmed (no way to run here): Linux and Windows compile or run, MSRV 1.91 build (toolchain not installed), the relayed path, real NAT, and the manual checklist on two machines.

## Measured results

| Item | Result |
|---|---|
| `cargo build --release -p axon` | OK, 2m15s |
| Release binary (`target/release/axon`, lto=true, macOS arm64) | **14,990,512 B = 14.30 MiB = 14.99 MB**. Under 15 MiB (15,728,640) by 738 KB; under 15 MB decimal by 9,488 B |
| Dist-profile binary (`target/dist/axon`, `lto="thin"`, what cargo-dist ships) | **20,106,080 B = 19.2 MiB = 20.1 MB. Over the 15 MB budget by ~5.1 MB (34%)** |
| Seams in release (source) | `seam()` returns `None` unless `cfg!(debug_assertions)`: `crates/axon-bus/src/fed/mod.rs:116-122`; readers at `fed/service.rs:66,452`, `fed/mod.rs:93` |
| Seams in release (binary) | `grep -a` finds 0 hits for `AXON_FED_RELAY`, `AXON_FED_BIND`, `AXON_TEST_NOW_OFFSET_MS`. Run with all three set: invite carries `relay_url` `https://euc1-1.relay.n0.iroh.link./` and public and LAN addresses (relay not disabled, bind not loopback), and `expires_at` is now+600 s (offset of 3,600,000 ms ignored). **Compiled out, confirmed.** |
| `fed-e2e.sh` run 1 | exit 0, all 17 steps PASS plus speed and size. 9 samples: p50 629 ms, p95 855 ms, max 855 ms. Question 641 ms, resume send 642 ms |
| `fed-e2e.sh` run 2 | exit 0, all PASS. p50 626 ms, p95 919 ms, max 919 ms. Question 661 ms, resume send 620 ms |
| Login link (release, `axon bus serve`, curl POST `/api/session`, Origin matching) | Fresh nonce: 204 plus `axon_session` cookie (`HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000`). Reuse: 401. Wrong Origin: 403. Unused nonce exchanged at 61 s: 401. Nonce exchanged at ~55 s: 204. No cookie on `/api/fed`: 401 |
| Federation on via API (`PUT /api/settings/federation`) | 200, fingerprint and node id appear, invite (`POST /api/fed/invites`) returns `axon1:` link with `expires_at` +600 s. `axon bus doctor` from an installed-path binary prints `federation: on (0 peers)` |
| Data modes | `axon/` 0700, `axon.db` 0600, `fed/` 0700, `identity.key` 0600; `fed/service.lock` **0644** |
| Cross-target check | Not possible: `x86_64-unknown-linux-gnu` is installed but `ring` needs a cross C compiler (`x86_64-linux-gnu-gcc` missing). `x86_64-pc-windows-gnu` is not installed (`can't find crate for core`). Nothing installed |
| Dependency MSRV | `cargo metadata`: no locked package declares a rust-version above 1.91 |

e2e speed note: the script times a debug-build `axon bus send` process start through the receiver's inbox commit, 9 samples, so p95 equals the max. It passes the 1 s target by 81-145 ms on an idle loopback.

## CI coverage (`.github/workflows/ci.yml`)

- OS matrix: `macos-latest`, `macos-15-intel`, `ubuntu-latest`, `ubuntu-24.04-arm`, `windows-latest` (ci.yml:27). Covers the three OSes. Windows runs `--test-threads=1`.
- New suites (`acceptance_fed_auth|lifecycle|pairing|shares|wire`, `acceptance_settings`, unit tests under `src/fed`) run through `cargo test --workspace --all-targets --no-fail-fast`. No per-suite step, but all are included.
- MSRV: `dtolnay/rust-toolchain@1.91` plus `cargo check --workspace --locked` (ci.yml:21-22); `rust-version = "1.91"` in Cargo.toml:7. See REL-4 for the gap.
- Clippy and fmt: ubuntu only (so Windows and macOS-only `cfg` code is never linted).
- Not in CI: `scripts/fed-e2e.sh` and any run of the bare `axon` binary. See REL-2.

## `cfg(unix)` and platform paths in `git diff main...job/fed`

| Location | Windows | Linux | Verdict |
|---|---|---|---|
| `fed/identity.rs:72,86,95` modes 0600/0700 (`restrict_to_owner`) | `#[cfg(windows)]` branch at :108 runs `icacls /inheritance:r /grant:r %USERNAME%:(F)` before the seed is written. Same function, no compile hole | native | Compiles on both, by reading. The Windows branch has no test and was never executed here (REL-5) |
| `fed/identity.rs:29,48,63` symlink refusal | `symlink_metadata` is portable, runs | runs | Fine. Its tests at :220, :247 are `cfg(unix)` (compiled out on Windows, no skip message) |
| `fed/lock.rs` `File::try_lock` | std 1.89+, works on Windows | works | Fine at MSRV 1.91 |
| `serve.rs:120` `OpenOptionsExt::mode(0o600)` on the ready-file temp | `cfg(unix)` gate, file created with default ACL on Windows | native | Compiles. Ready file on Windows is not owner-restricted (holds only the loopback URL) |
| `tests/common/fed.rs:424-450` `Stopped` (SIGSTOP/SIGCONT through `/bin/kill`) | Compiled out | `/bin/kill` exists on Linux and macOS | Compiled out. The header of `acceptance_fed_lifecycle.rs:5-6` explains it; the test run prints nothing |
| `acceptance_fed_lifecycle.rs:116,255,349,406` and `acceptance_fed_wire.rs:391` (blackhole/offline cases) | Compiled out | run | Windows has no blackhole fault test, so "Offline within 30 s" is untested there |
| `tests/common/faults.rs:2-16` unreadable database (`chmod 000`) | Compiled out. Its imports of `fed::*`, `Server`, `Duration` stay unconditional and become unused on Windows, which is a warning only (clippy `-D warnings` runs on Linux only) | Self-skips when run as root ("runner bypasses Unix file permissions") with an eprintln | OK |
| `acceptance_fed_pairing.rs:338` modes and symlink | Compiled out | run | A29's automated half is Unix-only |
| `acceptance_codex_observed.rs:5` `#![cfg(unix)]` plus `rustc`-built fixture (:40-55) | Whole file compiled out (0 tests, silently) | Needs `rustc` on PATH at test time: true on CI; it writes a copy, not a symlink | OK. Fixture mode 0755 is Unix API |
| `acceptance_codex_hooks.rs:391` | Compiled out | run | OK |
| `fed/envelope.rs:36-52` refs regex `[A-Za-z0-9._/-]`, rejects `\` and `C:` | Windows-style refs are refused, so agents there must write `/` | native | Behaviour, not a build break (REL-13) |
| `fed/shares/tests.rs`, `fed/delivery/tests.rs` fixtures `Path::new("/repo")`, `"/bin/axon"` | Used only as in-memory strings. `shares.rs:101` `is_absolute()` is false for `/repo` on Windows, so the result could differ if a path reaches it | fine | Unverified on Windows, depends on CI |

Nothing found that fails to compile on Linux or Windows by reading, but no cross-compile was possible. Windows-only `cfg(windows)` code in `identity.rs` and `snapshot.rs` is unchecked by any local compile.

## Findings

### REL-1 (P0): bare `axon` panics at startup, duplicate `GET /api/health`
- **Location:** `src/server.rs:43` (`.route("/api/health", get(api_health))` in `build_router`) merged with `crates/axon-bus/src/serve.rs:102` (`.route("/api/health", get(health))`, added in `7bc5db9`, 2026-10-02 01:11). `src/main.rs:~126-141` builds both.
- **Happened:** `axon --port 7791 --no-open` (release) and `target/debug/axon --port 7792 --no-open` both print `Dashboard: http://127.0.0.1:<port>/#login=...` and `live (file-watch)`, then panic: `axum-0.7.9/src/routing/path_router.rs:70:22: Overlapping method route. Handler for GET /api/health already exists`. The process dies, so the printed link is dead. Same on debug and release.
- **Expected:** README Usage and Privacy: `axon` serves the dashboard and the login link works.
- **Why nothing caught it:** `fed-e2e.sh` and the acceptance suites start `axon bus serve`, never the root binary's `run_server` / `build_router` (see REL-2). Every walkthrough step after this one used `axon bus serve` as a workaround.
- **Solved when:** `axon --port N --no-open` stays up 10 s; `GET /api/health` with the owner cookie returns 200 and the usage and dashboard health payloads are not both lost (decide which handler owns the route and say so in `server.rs:9`); a test launches the root binary (or builds `build_router`) and fails on a duplicate route.

### REL-2 (P1): no test or CI step exercises the root `axon` binary's server path
- **Location:** `.github/workflows/ci.yml:25-40`, `scripts/fed-e2e.sh:61` (uses `axon bus serve`), `tests/` (fixtures only).
- **Happened:** REL-1 shipped to the branch tip with all suites green. `fed-e2e.sh` runs on no CI OS.
- **Expected:** at least one per-OS check starts `axon --no-open --port 0`-style and fetches `/api/health`; the e2e script runs in CI on Linux and macOS.
- **Solved when:** a CI job starts the real `axon`, exchanges a login link, hits one authenticated route; `fed-e2e.sh` is a CI step (debug build, Linux plus macOS); the job fails if REL-1 is reintroduced.

### REL-3 (P1): the shipped (dist-profile) binary is 20.1 MB; the 15 MB budget is met only by the release profile, and only just
- **Location:** `Cargo.toml:65-72` (`[profile.release] lto=true`, `[profile.dist] lto="thin"`), `docs/P2P-PLAN.md:184`, `scripts/fed-e2e.sh:21,244-253` (measures `target/release`).
- **Happened:** `cargo build --profile dist -p axon` gives 20,106,080 B. Release is 14,990,512 B, 9,488 B under 15,000,000. `dist-workspace.toml` builds with the dist profile, so users get the large one. The script's budget is 15 MiB while the plan says 15 MB; on the decimal reading there is no margin. Linux musl and Windows sizes are unmeasured. I did not build `main` for a baseline, so the delta caused by iroh is not known.
- **Expected:** size measured on what ships, one unit for the budget (plan: "raising the limit is the owner's call").
- **Solved when:** the size check runs against the dist profile per target in CI (or the dist profile uses `lto=true`/`codegen-units=1`); plan and script state the same unit; the owner either accepts a new limit in `P2P-PLAN.md` §4 or the number comes under 15 MB.

### REL-4 (P2): MSRV job does not check test code or build with 1.91 locally
- **Location:** `ci.yml:21-22` (`cargo check --workspace --locked`, no `--all-targets`).
- **Happened:** the new suites and `dev-dependencies` are never compiled at 1.91; 1.91 is not installed here, so this is unverified. Dependency `rust-version`s are all at or below 1.91 (verified).
- **Solved when:** the msrv step uses `--all-targets`, or a decision records that tests are not held to the MSRV.

### REL-5 (P2): Windows security and fault paths have no executed test
- **Location:** `fed/identity.rs:108-124` (icacls), `fed/identity.rs:220-260`, `acceptance_fed_pairing.rs:338`, `tests/common/fed.rs:424-450`, `acceptance_fed_lifecycle.rs:116,255,349,406`.
- **Happened:** on Windows the owner-only key permission, the symlink refusal and the blackhole/offline cases are compiled out or only asserted by reading. Nothing prints a skip. `icacls` grants `%USERNAME%` only (domain accounts and `USERNAME` unset are untested; the function fails closed with an error if it is unset).
- **Expected:** each omission is visible as a skip, and the Windows ACL path has one test.
- **Solved when:** a `cfg(windows)` test asserts the key's ACL has one entry for the user (`icacls` output), and the skipped cases are listed in `FED-COVERAGE.md` with the reason.

### REL-6 (P2): README never says how to enable federation by API, so "Turn it on" cannot be scripted from the README
- **Location:** `README.md:111-127`.
- **Happened:** the only API pointer is "`/api/fed`, see `crates/axon-bus/tests/acceptance_m5.rs`", and that file has no `/api/fed` (grep: 0). The real calls are `PUT /api/settings/federation {"enabled":true}`, `POST /api/fed/invites {}`, `POST /api/fed/join {"invite","label"}`, `POST /api/fed/peers/<id>/confirm {"pair_code"}`. A POST without `Content-Type: application/json` returns 415, one without a matching `Origin` returns 403, one without the cookie returns 401; none of this is in the README.
- **Solved when:** the README (or a linked doc) lists those routes, the required `Origin`, `Content-Type` and cookie, with the right test file named (`acceptance_fed_pairing.rs`, `acceptance_fed_auth.rs`).

### REL-7 (P2): `docs/P2P-SPEC.md:453` says there is no Settings UI for invite, join, confirm, share
- **Happened:** the amendment predates the UI; `ui/settings-pair.js`, `settings-peers.js`, `settings-shares.js` implement it and README step 2-4 describes it. The same section says A34 is "a gap against the plan".
- **Solved when:** the amendment is removed or marked closed with the commit; A34 status stated once.

### REL-8 (P2): `docs/P2P-PLAN.md:171` names `cargo test --test acceptance_fed`, which does not exist
- **Happened:** `cargo test --test acceptance_fed` lists only the m1-m3 fixtures as valid targets. The suites are `acceptance_fed_{auth,lifecycle,pairing,shares,wire}`.
- **Solved when:** the plan names a command that runs them (`cargo test -p axon-bus --test 'acceptance_fed_*'` or the five names) and it exits 0.

### REL-9 (P2): FED-MANUAL cannot be followed as written (details in the review below)
- **Location:** `docs/FED-MANUAL.md` (steps `axon bus peers`, 3 and 6). See "Manual checklist review".
- **Solved when:** the gaps listed there are closed.

### REL-10 (P3): port 7777 in use gives a dead link and a bare error
- **Location:** `src/main.rs:~128-140`: the link and `live (file-watch)` are printed before `server::serve` binds.
- **Happened:** with another Axon on 7777 (this machine), output is a `#login=` link, "live", then `Error: bind 127.0.0.1:7777 / Address already in use (os error 48)`. The nonce is wasted and no hint to use `--port`.
- **Solved when:** the bind happens before the link is issued, and the error names `--port` or `axon open`.

### REL-11 (P3): `axon bus budget set <root> ...` in README Usage: `<root>` is a registered agent id, not a path
- **Happened:** with a project path: `axon-bus: agent <path> is not registered`. A new user has no registered agent.
- **Solved when:** README says what `<root>` is (or shows `axon bus register` first).

### REL-12 (P3): a build run from `target/` reports "development build" and wires no hooks
- **Happened:** `Hooks not wired: ... is a development build` (correct and documented for `cargo install`). `axon bus doctor` from such a path exits after that line without a `federation:` line. FED-MANUAL section 6 needs hooks and does not say to use an installed binary.
- **Solved when:** FED-MANUAL step 0 says "install with the release installer or `cargo install --path .`; a `target/` build wires no hooks".

### REL-13 (P3): refs with `\` or drive letters are refused (`fed/envelope.rs:36-52`)
- **Happened:** Windows agents must write `/`-style refs; a refused ref gives no hint in the README.
- **Solved when:** `axon bus guide` says refs use `/` separators on every OS.

### REL-14 (P3): `fed/service.lock` is 0644, every other file in `fed/` is 0600
- **Location:** `fed/lock.rs:20-25` (`OpenOptions` without mode). Contains only `pid N`; the 0700 directory contains it, so no leak.
- **Solved when:** created 0600, or the doc says the lock is not secret.

### REL-15 (P2): e2e script prerequisites and numbers are undocumented and thin
- **Location:** `scripts/fed-e2e.sh`.
- **Happened:** needs bash, curl, python3 (timing and JSON), git; not stated in README or P2P-PLAN. Runs on a debug build only (release has no seams, by design). p95 of 9 samples equals the max. No relayed-path run exists in script or CI; the ≤2 s relayed target is measured nowhere automatically.
- **Solved when:** prerequisites in the script header and README line; at least 20 samples for p95; relayed ≤2 s stays on the manual list with a defined way to measure.

## Fresh-user walkthrough: doc steps wrong, missing or confusing

Followed README as a first-time user, isolated HOME/XDG, port 7791 (7777 held by a running Axon of the owner, which I left alone).

| README step | Result |
|---|---|
| `axon` (Usage) | **Panics after printing the login link** (REL-1) |
| `axon --port 8080 --no-open` | Same panic (REL-1) |
| `axon open --print` | Works (`bus serve` was running): fresh `Dashboard: http://127.0.0.1:7791/#login=...` each call |
| Login link 60 s | Verified (see table above) |
| Privacy: "SQLite 0600 in a 0700 directory" | True |
| `axon --scan-only` | Prints the JSON summary (zeros on an empty HOME) |
| `axon bus doctor` | Works from an installed-path binary; federation line present |
| `axon bus guide` | Works |
| `axon bus budget set <root> 2Mtok --usd 20` | Needs a registered agent id (REL-11) |
| `~/.config/axon/config.toml`, `assets/config.example.toml`, `pricing.toml` paths | Files exist in repo (assets/config.example.toml, crates/axon-core/assets/pricing.toml). I did not exercise the override paths |
| Working across machines step 1 | API exists but undocumented (REL-6). UI labels in README all exist in `crates/axon-bus/ui/*.js` ("Invite a machine", "Join with an invite", "Cancel invite", "Copy", "Confirm", "Reject", "Share a project", "Send"/"Receive", "Unshare", "Pause", "Resume", "Disconnect", states Connected/Reconnecting/Offline/Paused/Awaiting confirmation) |
| Step 2 "the same steps are available over the local API (see acceptance_m5.rs)" | Wrong file (REL-6) |
| Not verified | The browser UI itself (no browser), installers and brew, the countdown, Copy button |

## Manual checklist review (`docs/FED-MANUAL.md`)

Verdict: executable by two people with the gaps below fixed; today it is close but a tester will stall in at least five places.

Strengths: seven sections each map to a `P2P-SPEC` §11 row (A29, A30, relayed path); numeric targets are given for direct (1 s) and relayed (2 s) and for offline detection (30 s); screenshots are requested per state; "a line that fails is a finding" is stated.

Gaps:
1. **No step 0 / prerequisites.** Build version match is stated, but not: install by installer or `cargo install` (REL-12), the dashboard login (`axon open --print`), which port each side uses, or that the owner must send the invite by a channel.
2. **`axon bus peers` (section 2) fails as written.** It requires `--agent <AGENT>`; the manual also does not say how to know the agent id.
3. **Timing is not measurable.** "Record the time from send to the message reaching the other agent's context" has no method. Provide one: the sender's `queued` time and the Peers panel round-trip, or the script `send_timed` approach, and say what to compare.
4. **Relayed path is not concrete.** "Block UDP ... firewall rule for `axon`, or a network that drops it" gives no command. Needed: a macOS `pfctl` rule, a Linux `iptables`/`nft` line, a Windows `netsh advfirewall` line, plus how to confirm the block (e.g. direct path flips to `relay` in the panel and which field shows it). The relay is the default n0 relay (needs internet), and the expected result "Connected with path `relay`" does not say how long to wait. Return to direct: "note how long it took" with no bound; a target is missing.
5. **NAT is not concrete.** "Use two different home networks, behind ordinary NAT" does not ask each tester to record NAT type, whether hole-punching worked, or what the panel shows if both sides are on CGNAT (the first outcome that tends to force relay). Add a field in Record: path, RTT and how long to first direct.
6. **Expected results missing or loose:** section 1 "A deliberately wrong code ... removes the pairing on both" (how to type a wrong code, since the panel shows only Confirm and Reject; the UI says Confirm has a code entry, so say where); section 4 "queue count and bytes grow" without values; "delivered once each" without how to count (use the Peers counters `received`); section 5 refusal strings `refused: peer_removed` stated, but `peer_paused` for step in section 2 not.
7. **Section 3 restarts "the Federation switch"** but does not say that the invite or pairing survives; no check that the pairing key fingerprint is unchanged across a restart.
8. **Section 5 line is overloaded** (one bullet holds four statements and a spec reference); split so each has a pass or fail.
9. **Section 6** says to look for `[remote message ...]` with `│ ` prefixes, with no way to trigger a hook turn. Say which command or prompt makes the harness show the context.
10. **Section 7 A29** needs the second OS account steps (`su`/fast-user-switch, `ls -l`, `curl` with no cookie expecting 401), not just the expected end state. **A30** says "cut power or kill -9"; pick one and state the expected `queued`/`delivered` evidence (`axon bus` command or API) and how to confirm "stored once".
11. No time box or ordering for the 10-minute invite and the 30 s offline window, and no cleanup (remove each Axon's test share and delete the pairing).
12. No place to record pass or fail in a form that can be filed; it asks for a finding per unticked line but gives no id format.

## Order to fix

1. REL-1 (blocker).
2. REL-2 (so it cannot recur) and REL-3 (decide the dist-profile size and unit).
3. REL-6, REL-7, REL-8, REL-9, REL-15, REL-4, REL-5 (documentation and coverage honesty).
4. REL-10 to REL-14.

Leftovers: the walkthrough scratch directory under `$TMPDIR/walk` was not deleted (the removal was blocked by a safety check; it holds a throwaway DB and key). Other `axon-bus serve` processes on this machine (pids 18655, 83974, 84010) belong to other sessions or test runs; I did not touch them.
