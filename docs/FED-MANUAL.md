# Federation: manual two-machine checklist

For the owner and one teammate, each on their own machine. It covers the rows of
`docs/P2P-SPEC.md` §11 that no automated test can reach: a real network path, a relay, a
second operating system. `scripts/fed-e2e.sh` already covers the flow on loopback. Tick each
line and note the observed numbers beside it; a line that fails is a finding, not a retry.

Both machines run the same build of `axon` (`axon --version` matches), each with its own
home directory. Use two different home networks, behind ordinary NAT.

## 1. Pair across real networks
- [ ] Both: Settings, Federation on. The Peers panel says there are no peers.
- [ ] One side creates an invite, sends it over a chat; the other joins with it.
- [ ] Both screens show the same pair code and the other side's fingerprint. Read the code
      aloud. A deliberately wrong code on one side removes the pairing on both; pair again.
- [ ] Confirm on both sides. Both show Connected. Record the path (direct or relay) and the
      round-trip time shown.
- [ ] Settings screenshot of the Peers panel in the Connected state, on each machine.

## 2. Direct path (home NAT)
- [ ] Share a project each way (each owner turns inbound and outbound on). Each agent in the
      project appears to the other as `peer:<label>/<session>` in `axon bus peers`.
- [ ] Send a `question` each way and answer it. Record the time from send to the message
      reaching the other agent's context. Target: 1 s or less on a direct path.
- [ ] Pause on one side. Sends from either side stop being delivered; the panel shows Paused
      here and the other side shows no error. Resume: queued messages arrive.

## 3. Relayed path
- [ ] Block UDP on one machine (firewall rule for `axon`, or a network that drops it).
      Restart the Federation switch. The peer returns to Connected with path `relay`.
- [ ] Repeat the question and answer. Target: 2 s or less relayed. Record it.
- [ ] Unblock UDP. The path returns to `direct` without a restart; note how long it took.
- [ ] Settings screenshot of the Peers panel showing a relayed path.

## 4. Failure and recovery
- [ ] Disconnect one machine from the network. The other shows Reconnecting, then Offline
      within 30 s, with a next-try time. Queue count and bytes grow as messages are sent.
- [ ] Reconnect. State returns to Connected and the queued messages arrive once each.
- [ ] Quit `axon` on one machine, start it again: the pairing and shares are still there.
- [ ] Settings screenshots of Reconnecting and Offline.

## 5. Disconnect and re-pair
- [ ] Disconnect on one side (the in-page confirm appears first). The peer disappears there
      and its shares end; messages from the other side are refused there (not an active peer). The other side keeps showing the peer until its owner disconnects too; a removed notice is only acknowledged (spec §12).
- [ ] The disconnected side's agents get `refused: peer_removed` on a send to the old target.
- [ ] Pair again: a new pairing starts with no shares and no old messages delivered.

## 6. Two operating systems
- [ ] One machine macOS, one Linux or Windows. Repeat sections 1, 2 and 5.
- [ ] Hooks deliver a remote message into each harness in use (Claude Code, Codex): the
      context shows the `[remote message …]` block with each body line prefixed by `│ `.
- [ ] Note anything that differs by OS (paths in the lock file, firewall prompts).

## 7. Local account separation (A29) and power loss (A30)
- [ ] On a machine with a second user account: that account cannot read the first user's Axon
      data directory or the identity key (modes `0700` and `0600`), and the dashboard shows it
      only a sign-in screen.
- [ ] While a message is queued to an offline peer, cut power or kill -9 `axon`. After restart
      the message is still queued and is delivered once when the peer returns; no message is
      stored twice on the receiver.

## Record
Machine and OS of each side, `axon --version`, the numbers above, and the screenshots. File a
finding for every unticked line.
