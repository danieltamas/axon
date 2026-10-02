# Federation: manual two-machine checklist

For the owner and one teammate, each on their own machine. It covers the rows of
`docs/P2P-SPEC.md` §11 that no automated test can reach: a real network path, a relay, a
second operating system. `scripts/fed-e2e.sh` already covers the flow on loopback. Tick each
line and note the observed numbers beside it; a line that fails is a finding, not a retry.

Both machines run the same build of `axon` (`axon --version` matches), each with its own
home directory. Use two different home networks, behind ordinary NAT.

## 0. Before you start
- [ ] Install with the release installer or `cargo install --path .`. A binary run from
      `target/` is a development build: it wires no hooks, so section 6 cannot pass.
- [ ] Open the dashboard with `axon open --print` (a login link; the page keeps its session
      token in the browser, so use one browser per Axon). Note each side's port.
- [ ] Register one agent per machine in the project you will share and note its id (the
      dashboard's agent list shows it, or `axon bus register --id <agent> ...` makes one). The
      checks below use `<agent>` for it.
- [ ] Timing: the invite lives 10 minutes and a peer reads Offline after 30 s without an
      answer, so keep sections 1 and 4 within those windows. At the end, Disconnect on both
      sides and unshare, so the next run starts clean.
- [ ] Record each side's NAT type if you know it, and whether the path first came up
      `direct` or `relay` and after how many seconds.

## 1. Pair across real networks
- [ ] Both: Settings, Federation on. The Peers panel says there are no peers.
- [ ] One side creates an invite, sends it over a chat; the other joins with it.
- [ ] Both screens show the same pair code and the other side's fingerprint. Read the code
      aloud. The server only records that each owner attests the match, so compare out loud.
- [ ] Confirm on both sides. Both show Connected. Record the path (direct or relay) and the
      round-trip time shown.
- [ ] Settings screenshot of the Peers panel in the Connected state, on each machine.

## 2. Direct path (home NAT)
- [ ] Share a project each way (each owner turns inbound and outbound on). Each agent in the
      project appears to the other as `peer:<label>/<session>` in `axon bus peers --agent
      <agent>`.
- [ ] Send a `question` each way and answer it. Time it as `scripts/fed-e2e.sh`
      does: note the clock when `axon bus send` prints `queued`, then poll the other side's
      `GET /api/fed` until that peer's `counters.received` goes up, and note the clock. The
      difference is the send-to-inbox time. Target: 1 s or less on a direct path.
- [ ] Pause on one side. Sends from either side stop being delivered; the panel shows Paused
      here, and the other side shows "Paused by <label>", Offline, with no error. A send from
      the other side prints `refused: peer_paused`. Resume: the notice clears there and queued
      messages arrive.

## 3. Relayed path
- [ ] Block UDP for `axon` on one machine. macOS: a `pfctl` rule `block drop out proto udp
      from any to any`; Linux: `nft add rule inet filter output meta skuid <user> udp
      dport != 53 drop`; Windows: `netsh advfirewall firewall add rule name=axon-block
      dir=out action=block protocol=UDP program=<axon.exe>`. Restart the Federation switch.
      Within 60 s the peer returns to Connected and its path reads `relay` (the Peers
      panel's path field). The default relay needs internet access.
- [ ] Repeat the question and answer. Target: 2 s or less relayed. Record it.
- [ ] Unblock UDP. The path returns to `direct` without a restart; note how long it took
      (target: 2 minutes or less).
- [ ] Settings screenshot of the Peers panel showing a relayed path.

## 4. Failure and recovery
- [ ] Disconnect one machine from the network. The other shows Reconnecting, then Offline
      within 30 s, with a next-try time. Send three messages: the queue shows count 3 and their total bytes.
- [ ] Reconnect. State returns to Connected and the queued messages arrive once each (the other side's `counters.received` goes up by
      exactly three).
- [ ] Quit `axon` on one machine, start it again: the pairing and shares are still there,
      and the key fingerprint on the Peers panel is unchanged.
- [ ] Settings screenshots of Reconnecting and Offline.

## 5. Disconnect and re-pair
- [ ] Disconnect on one side (the in-page confirm appears first). The peer disappears there
      and its shares end.
- [ ] The other side shows the peer as "Removed by them", its shares ended, with a Forget
      button; Forget removes the card.
- [ ] The disconnected side's agents get `refused: peer_removed` on a send to the old target.
- [ ] Pair again: a new pairing starts with no shares and no old messages delivered.

## 6. Two operating systems
- [ ] One machine macOS, one Linux or Windows. Repeat sections 1, 2 and 5.
- [ ] Hooks deliver a remote message into each harness in use (Claude Code, Codex): send a
      message, then give the receiving agent any prompt, and its context shows the `[remote message …]` block with each body line prefixed by `│ `.
- [ ] Note anything that differs by OS (paths in the lock file, firewall prompts).

## 7. Local account separation (A29) and power loss (A30)
- [ ] On a machine with a second user account: `ls -l` shows the first user's Axon data
      directory is `0700` and the identity key `0600`, the second account cannot read either,
      and `curl http://127.0.0.1:<port>/api/fed` without the cookie and token answers 401.
- [ ] While a message is queued to an offline peer, `kill -9` `axon`. After restart
      `GET /api/fed` shows it still queued, and it is delivered when the peer returns: the
      peer's `counters.received` goes up by one, not two.

## 8. The same steps over the local API
Everything the Settings page does is a route, for scripting a run or driving a headless
machine. Sign in with `axon open --print`: `POST /api/session` `{"nonce":"<from the link>"}`
answers `{"token":"…"}` and sets the cookie. Every call then needs both (`Cookie:
axon_session=…` and `x-axon-session: <token>`), a matching `Origin` and, on POST and PUT,
`Content-Type: application/json`.

- `PUT /api/settings/federation {"enabled":true}` turns federation on.
- `POST /api/fed/invites {}` creates an invite; `POST /api/fed/join {"invite","label"}` joins
  with one.
- `POST /api/fed/peers/<id>/confirm {"pair_code"}` attests the pair code on this side.
- `GET /api/fed` reads peers, shares, health and counters.

The acceptance tests `crates/axon-bus/tests/acceptance_fed_pairing.rs` and
`acceptance_fed_auth.rs` exercise every route above.

## Record
Machine and OS of each side, `axon --version`, the numbers above, and the screenshots. File a
finding for every unticked line, named `FED-<section>-<line>` (for example `FED-3-1`) with
pass or fail and the numbers.
