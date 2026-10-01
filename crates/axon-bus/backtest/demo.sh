#!/usr/bin/env bash
# The dashboard's design fixture (BUS-PLAN §7-§8): a throwaway hub with two repos (one
# with a worktree), four harnesses, a Claude orchestrator with subagents, a root link
# across repos, budgets near their ceilings, messages and narrative.
#
#   demo.sh seed DIR    build the hub under DIR (wiped first)
#   demo.sh serve DIR   serve it with content capture on (prints the URL)
#   demo.sh live DIR    keep sending messages and usage so the page moves (Ctrl-C stops)
#
# Registry, links, messages and budgets go through the bus CLI. Usage rows, hook-driven
# status and the capture lease have no CLI (hooks, ingest and `serve` write them), so
# they are written with sqlite3 into this throwaway hub only.
set -euo pipefail

cmd=${1:?usage: demo.sh seed|serve|live DIR}
root=$(mkdir -p "${2:?DIR required}" && cd "$2" && pwd)
bus=${AXON_BUS:-$(cd "$(dirname "$0")/../../.." && pwd)/target/debug/axon-bus}
export XDG_DATA_HOME="$root/data" XDG_CONFIG_HOME="$root/config" XDG_CACHE_HOME="$root/cache"
db="$XDG_DATA_HOME/axon/axon.db"
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
sql() { sqlite3 -cmd '.timeout 2000' "$db" "$@"; }

usage_row() { # agent model input output cache_read
  sql "INSERT INTO usage (agent_id,ts,model,input_tokens,output_tokens,cache_read_tokens,received_at)
       VALUES ('$1',$(now_ms),'$2',$3,$4,$5,$(now_ms));"
}

seed() {
  rm -rf "$root" && mkdir -p "$root"/{data,config,cache,repos}
  local axon="$root/repos/axon" web="$root/repos/gamerina-web" wt="$root/repos/axon-m5"
  for repo in "$axon" "$web"; do
    git init -q --initial-branch=main "$repo"
    git -C "$repo" -c user.name=demo -c user.email=demo@example.invalid commit -q --allow-empty -m init
  done
  git -C "$axon" worktree add -q -b job/axon-bus/m5 "$wt"
  "$bus" init >/dev/null

  reg() { # id harness session cwd parent role model
    "$bus" register --id "$1" --harness "$2" --session "$3" --cwd "$4" \
      ${5:+--parent "$5"} ${6:+--role "$6"} ${7:+--model "$7"} >/dev/null
  }
  reg orch-7f3a   claude   s-orch   "$axon" "" orchestrator claude-opus-4-8
  reg coder-a1    claude   s-orch   "$axon" orch-7f3a coder claude-sonnet-4-6
  reg security-b2 claude   s-orch   "$axon" orch-7f3a security claude-opus-4-8
  reg qa-c3       claude   s-orch   "$axon" orch-7f3a qa claude-haiku-4-5
  reg probe-d4    claude   s-orch   "$web"  orch-7f3a researcher claude-fable-5
  reg codex-9e21  codex    s-codex  "$wt"   "" reviewer gpt-5.5
  reg codex-t1    codex    s-codex-t1 "$wt" codex-9e21 tests gpt-5.4
  reg web-orch    claude   s-web    "$web"  "" orchestrator claude-opus-4-8
  reg oc-4410     opencode s-oc     "$web"  "" builder openrouter/auto
  reg oc-scout    opencode s-oc-scout "$web" oc-4410 scout openrouter/auto
  reg hermes-ops  hermes   s-hermes "$root" "" ops claude-sonnet-4-6

  sql "UPDATE agents SET status='active' WHERE id IN ('orch-7f3a','coder-a1','probe-d4','codex-9e21','oc-4410','hermes-ops');
       UPDATE agents SET status='closed', ended_at=$(now_ms) WHERE id='qa-c3';
       UPDATE agents SET mission='Ship M5: dashboard design pass against the replay' WHERE id='orch-7f3a';
       UPDATE agents SET mission='Rebuild the snapshot keyed renderer' WHERE id='coder-a1';
       UPDATE agents SET mission='Cross-vendor review of the M5 diff' WHERE id='codex-9e21';
       UPDATE agents SET mission='Checkout flow copy and layout' WHERE id='oc-4410';"

  usage_row orch-7f3a claude-opus-4-8 180000 42000 910000
  usage_row coder-a1 claude-sonnet-4-6 260000 88000 1400000
  usage_row security-b2 claude-opus-4-8 90000 12000 300000
  usage_row qa-c3 claude-haiku-4-5 40000 6000 120000
  usage_row probe-d4 claude-fable-5 30000 9000 60000
  usage_row codex-9e21 gpt-5.5 210000 31000 640000
  usage_row codex-t1 gpt-5.4 70000 15000 200000
  usage_row web-orch claude-opus-4-8 20000 4000 50000
  usage_row oc-4410 openrouter/auto 150000 40000 220000
  usage_row oc-scout openrouter/auto 30000 5000 40000
  usage_row hermes-ops claude-sonnet-4-6 60000 11000 90000

  "$bus" budget set orch-7f3a 5Mtok --usd 40 >/dev/null
  "$bus" budget set oc-4410 480Ktok >/dev/null
  "$bus" budget set codex-9e21 2Mtok >/dev/null
  "$bus" link --from orch-7f3a --to web-orch >/dev/null
  "$bus" accept --from web-orch --to orch-7f3a >/dev/null

  # DEMO_SCALE=200 adds idle trees until the hub holds that many agents (§9 M5: 200).
  local n=11 tree=0
  while ((n < ${DEMO_SCALE:-0})); do
    tree=$((tree + 1))
    reg "bulk-$tree" codex "s-bulk-$tree" "$([ $((tree % 2)) = 0 ] && echo "$axon" || echo "$web")" "" worker gpt-5.5
    n=$((n + 1))
    for child in 1 2 3; do
      ((n < DEMO_SCALE)) || break
      reg "bulk-$tree-$child" codex "s-bulk-$tree-$child" "$web" "bulk-$tree" tests gpt-5.4
      n=$((n + 1))
    done
  done

  send() { "$bus" send --from "$1" --to "$2" --kind "$3" --body "$4" ${5:+--thread "$5"} >/dev/null; }
  send orch-7f3a coder-a1 handoff "Rebuild the tree renderer keyed by agent id; keep the send form mounted across snapshots."
  send orch-7f3a security-b2 question "Does the snapshot leak narrative text when capture is off? Check the CASE gates."
  send security-b2 orch-7f3a answer "No: text and tool detail are NULL in SQL when content is off; the page never sees them."
  send orch-7f3a probe-d4 redirect "Stop crawling checkout; measure first paint on the dashboard instead."
  send coder-a1 orch-7f3a sync "Keyed renderer in; 200-agent replay holds frame budget locally."
  send orch-7f3a web-orch sync "Bus dashboard lands today; your checkout agents will show under gamerina-web."
  send web-orch orch-7f3a ack "Seen. oc-4410 is near its budget, expect a warn."
  send oc-4410 oc-scout stop "Budget at 92%: finish the current file and hand back."
  send codex-t1 codex-9e21 sync "3 red tests in TEST-9, all Codex config round-trips."
  sql "UPDATE messages SET delivered_at=sent_at+900, acked_at=sent_at+4200 WHERE kind<>'stop' AND to_id<>'probe-d4';"

  narrative
  echo "seeded $root"
}

narrative() {
  local file="$root/narrative.jsonl" t
  t=$(python3 -c 'import datetime as d; print((d.datetime.now(d.timezone.utc)-d.timedelta(minutes=9)).strftime("%Y-%m-%dT%H:%M:%S.000Z"))')
  cat >"$file" <<EOF
{"harness":"claude","session_id":"s-orch","record":{"type":"assistant","timestamp":"$t","sessionId":"s-orch","uuid":"n1","message":{"id":"m1","model":"claude-opus-4-8","content":[{"type":"thinking","thinking":"","signature":"opaque"},{"type":"text","text":"Plan for the M5 design pass: seed the replay fixture, take a baseline contact sheet, then run the impeccable pipeline step by step against it."},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"docs/BUS-PLAN.md"}},{"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"crates/axon-bus/ui/app.js"}},{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"cargo build -p axon-bus"}}],"usage":{"input_tokens":1200,"output_tokens":300}}},"reasoning_tokens":412}
{"harness":"claude","session_id":"s-orch","record":{"type":"assistant","timestamp":"$t","sessionId":"s-orch","uuid":"n2","message":{"id":"m2","model":"claude-opus-4-8","content":[{"type":"thinking","thinking":"Delegating the renderer rewrite to coder-a1; security-b2 checks the capture gates in parallel.","display":"updates","mode":"between_tools"},{"type":"tool_use","id":"t4","name":"Agent","input":{"subagent_type":"coder"}},{"type":"text","text":"Handed the keyed renderer to coder-a1 and the capture audit to security-b2. Waiting on both before the first shotbox pass."}],"usage":{"input_tokens":900,"output_tokens":220}}},"reasoning_tokens":88}
{"harness":"claude","session_id":"s-orch","record":{"type":"assistant","timestamp":"$t","sessionId":"s-orch","agentId":"coder-a1","uuid":"n3","message":{"id":"m3","model":"claude-sonnet-4-6","content":[{"type":"text","text":"Rows are now reconciled by agent id, so the active pulse no longer restarts on every snapshot."},{"type":"tool_use","id":"t5","name":"Edit","input":{"file_path":"crates/axon-bus/ui/app.js"}},{"type":"tool_use","id":"t6","name":"Bash","input":{"command":"cargo test -p axon-bus --test acceptance_m5"}}],"usage":{"input_tokens":800,"output_tokens":180}}},"reasoning_tokens":0}
{"harness":"codex","session_id":"s-codex","record":{"type":"response_item","payload":{"type":"reasoning","summary":[{"type":"summary_text","text":"Reviewing the snapshot facts: costs are gathered once per snapshot, not per node."}],"encrypted_content":"opaque"}},"reasoning_tokens":640}
{"harness":"codex","session_id":"s-codex","record":{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"One finding: links expire but the page keeps drawing them until the next snapshot. Minor; noting it for the report."}]}},"reasoning_tokens":0}
{"harness":"codex","session_id":"s-codex","record":{"type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"exec_command","arguments":"{\"cmd\":\"rg -n links crates/axon-bus/src\"}"}},"reasoning_tokens":0}
{"harness":"codex","session_id":"s-codex","record":{"type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"opaque"}},"reasoning_tokens":1210}
{"harness":"opencode","session_id":"s-oc","record":{"info":{"id":"o1","sessionID":"s-oc","role":"assistant","modelID":"openrouter/auto","providerID":"openrouter","time":{"created":$(now_ms)},"tokens":{"input":900,"output":200,"reasoning":150,"cache":{"read":0,"write":0}}},"parts":[{"id":"p1","type":"reasoning","text":"Budget is close; finish the checkout copy before touching layout."},{"id":"p2","type":"text","text":"Checkout copy rewritten in the house voice. Layout pass deferred until the budget is raised."},{"id":"p3","type":"tool","callID":"x1","tool":"edit","state":{"status":"completed","input":{"filePath":"src/checkout/Copy.tsx"},"output":"ok"}},{"id":"p4","type":"tool","callID":"x2","tool":"bash","state":{"status":"error","input":{"command":"npm test"},"output":"1 failed"}}]},"reasoning_tokens":150}
{"harness":"hermes","session_id":"s-hermes","record":{"role":"assistant","content":"Nightly backup verified; two stale worktrees flagged for repo-sync wt-gc.","reasoning_content":"Checked backup manifests before touching worktrees."},"reasoning_tokens":60}
EOF
  # Text is kept only while capture is on, as a running `serve --content` would hold it.
  sql "INSERT OR REPLACE INTO settings (key,value) VALUES ('content_capture_until','$(( $(now_ms) + 30000 ))');"
  "$bus" replay "$file" --speed 100000x >/dev/null
}

live() {
  local pairs=(
    "coder-a1|orch-7f3a|sync|Snapshot render under 4 ms at 200 agents."
    "orch-7f3a|coder-a1|redirect|Keep the SVG layer; drop the canvas experiment."
    "orch-7f3a|web-orch|sync|Design pass step done; contact sheet next."
    "web-orch|orch-7f3a|question|Can the dashboard show oc-4410's budget ring?"
    "codex-t1|codex-9e21|sync|TEST-9 green after the tables fix."
    "oc-scout|oc-4410|answer|Handed back: copy done, layout untouched."
    "security-b2|orch-7f3a|sync|Capture lease verified: lapses 30 s after serve stops."
    "orch-7f3a|probe-d4|question|First paint number on localhost?"
    "probe-d4|orch-7f3a|answer|62 ms cold, 18 ms warm."
  )
  local agents=(orch-7f3a coder-a1 codex-9e21 oc-4410 hermes-ops probe-d4)
  local models=(claude-opus-4-8 claude-sonnet-4-6 gpt-5.5 openrouter/auto claude-sonnet-4-6 claude-fable-5)
  # One stand-in process per harness session, holding some real memory, so the memory
  # figures come from sampled processes. Claude subagents share their session's process.
  local stand_ins=("orch-7f3a 64" "codex-9e21 48" "codex-t1 24" "web-orch 40" "oc-4410 56" "oc-scout 16" "hermes-ops 32")
  trap 'kill $(jobs -p) 2>/dev/null' EXIT
  for entry in "${stand_ins[@]}"; do
    set -- $entry
    # The process records its own pid: \$! can be a version-manager shim's.
    python3 - "$db" "$1" "$2" <<'PY' &
import os, sqlite3, sys, time
db, agent, mib = sys.argv[1], sys.argv[2], int(sys.argv[3])
held = b"\1" * (mib << 20)
with sqlite3.connect(db, timeout=2) as conn:
    conn.execute("UPDATE agents SET pid=? WHERE id=? OR (parent_id=? AND harness='claude')", (os.getpid(), agent, agent))
time.sleep(10**6)
PY
  done
  while true; do
    IFS='|' read -r from to kind body <<<"${pairs[RANDOM % ${#pairs[@]}]}"
    "$bus" send --from "$from" --to "$to" --kind "$kind" --body "$body" >/dev/null || true
    sql "UPDATE messages SET delivered_at=sent_at+700 WHERE delivered_at IS NULL AND kind<>'stop' AND sent_at < $(now_ms)-1500;"
    local i=$((RANDOM % ${#agents[@]}))
    usage_row "${agents[i]}" "${models[i]}" $((RANDOM / 4)) $((RANDOM / 40)) $((RANDOM / 2))
    sleep "${DEMO_EVERY:-1.2}"
  done
}

case $cmd in
  seed) seed ;;
  serve) exec "$bus" serve --content --port "${PORT:-7433}" ;;
  live) live ;;
  *) echo "usage: demo.sh seed|serve|live DIR" >&2; exit 2 ;;
esac
