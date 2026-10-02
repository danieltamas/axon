#!/usr/bin/env bash
# Two Axon instances on one machine, driven end to end:
#   pair -> share -> send both ways -> pause -> resume -> remove
# Prints PASS/FAIL per step, the measured send-to-inbox latency (docs/P2P-PLAN.md section 4:
# <= 1 s p95 on a direct path) and the shipped (dist profile) binary size against its 15 MB
# budget, in decimal megabytes.
#
# Needs a DEBUG build of `axon`: the loopback-only, no-relay seams (AXON_FED_RELAY,
# AXON_FED_BIND) do not exist in release builds, which would dial the public relays.
# Both instances live under one temp directory with their own HOME and XDG dirs; nothing
# under the real ~/.claude, ~/.codex, ~/.config or ~/.local/share/axon is read or written.
#
#   scripts/fed-e2e.sh                 build what is missing, run, exit 0 when every step passes
#   AXON=path/to/debug/axon            use another debug binary
#   FED_E2E_SKIP_SIZE=1                skip the dist build and size check

set -u
cd "$(dirname "$0")/.."

AXON=${AXON:-target/debug/axon}
SHIPPED=target/dist/axon
SIZE_BUDGET=15000000
LATENCY_BUDGET_MS=1000
SAMPLES=8

ROOT=$(mktemp -d "${TMPDIR:-/tmp}/axon-fed-e2e.XXXXXX")
FAILED=0

cleanup() {
  [ -n "${E2E_KEEP:-}" ] && return
  for pidfile in "$ROOT"/*/pid; do [ -f "$pidfile" ] && kill "$(cat "$pidfile")" 2>/dev/null; done
  wait 2>/dev/null
  case "$ROOT" in "${TMPDIR:-/tmp}"/axon-fed-e2e.*) rm -rf "$ROOT" ;; esac
}
trap cleanup EXIT

pass() { printf 'PASS  %s\n' "$1"; }
fail() {
  printf 'FAIL  %s\n' "$1"
  [ $# -gt 1 ] && printf '      %s\n' "$2"
  FAILED=1
  exit 1
}
check() { # name, command...: PASS when the command succeeds
  local name=$1
  shift
  if out=$("$@" 2>&1); then pass "$name"; else fail "$name" "$out"; fi
}
now_ms() { python3 -c 'import time; print(int(time.time() * 1000))'; }
json() { # an expression over the parsed stdin `d`: strings print bare, the rest as JSON
  python3 -c 'import json, sys
d = json.load(sys.stdin)
v = eval(sys.argv[1])
print(v if isinstance(v, str) else json.dumps(v))' "$1"
}

if [ ! -x "$AXON" ]; then cargo build -p axon || fail "build the debug binary"; fi
AXON=$(cd "$(dirname "$AXON")" && pwd -P)/$(basename "$AXON")

# --- one isolated instance -------------------------------------------------------------
# axon_<n> runs the CLI as that instance; api <n> METHOD PATH [BODY] talks to its dashboard.
isolated() { # instance, command...
  local n=$1
  shift
  env -i PATH="$PATH" HOME="$ROOT/$n/home" USERPROFILE="$ROOT/$n/home" \
    XDG_DATA_HOME="$ROOT/$n/data" XDG_CONFIG_HOME="$ROOT/$n/config" XDG_CACHE_HOME="$ROOT/$n/cache" \
    XDG_STATE_HOME="$ROOT/$n/data/state" CODEX_HOME="$ROOT/$n/home/.codex" \
    CLAUDE_CONFIG_DIR="$ROOT/$n/home/.claude" HERMES_HOME="$ROOT/$n/home/.hermes" \
    TMPDIR="$ROOT/$n/tmp" GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL="$ROOT/empty-gitconfig" \
    AXON_FED_RELAY=disabled AXON_FED_BIND=127.0.0.1:0 NO_COLOR=1 TZ=UTC LANG=C \
    "$@"
}
bus() { local n=$1; shift; (cd "$ROOT/$n/repo" && isolated "$n" "$AXON" bus "$@" 2>&1); }

api() { # instance, method, path, [body]: prints the response body, fails on non-2xx
  local n=$1 method=$2 path=$3 body=${4:-} url cookie token reply status
  url=$(cat "$ROOT/$n/url")
  cookie=$(cat "$ROOT/$n/cookie")
  token=$(cat "$ROOT/$n/token")
  reply=$(curl -s -w '\n%{http_code}' -X "$method" "$url$path" -H "Cookie: $cookie" \
    -H "x-axon-session: $token" -H "Origin: $url" -H 'Content-Type: application/json' ${body:+-d "$body"})
  status=${reply##*$'\n'}
  case $status in 2??) printf '%s' "${reply%$'\n'*}" ;; *) echo "$method $path -> $status ${reply%$'\n'*}" >&2; return 1 ;; esac
}

wait_for() { # description, timeout_s, command...: poll every 50 ms
  local what=$1 limit=$2
  shift 2
  local end=$(($(now_ms) + limit * 1000))
  while ! "$@" >/dev/null 2>&1; do
    [ "$(now_ms)" -lt "$end" ] || { echo "timed out waiting for $what"; return 1; }
    sleep 0.05
  done
}

start_instance() {
  local n=$1 agent=$2 harness=$3
  mkdir -p "$ROOT/$n"/{home,data,config,cache,tmp,repo}
  [ -f "$ROOT/empty-gitconfig" ] || : >"$ROOT/empty-gitconfig"
  (
    cd "$ROOT/$n/repo" || exit 1
    isolated "$n" git init -q --initial-branch=main
    isolated "$n" git -c user.name=e2e -c user.email=e2e@example.invalid commit -q --allow-empty -m "$n"
  ) || return 1
  local repo_path
  repo_path=$(cd "$ROOT/$n/repo" && pwd -P)
  echo "$repo_path" >"$ROOT/$n/repo-path"
  bus "$n" init >/dev/null || return 1
  bus "$n" register --id "$agent" --harness "$harness" --session "$agent" --cwd "$repo_path" >/dev/null || return 1
  # The shell records its pid, then becomes the server, so cleanup kills the server itself.
  (cd "$ROOT/$n/repo" && isolated "$n" sh -c 'echo $$ >"$1"; shift; exec "$@"' sh "$ROOT/$n/pid" \
    "$AXON" bus serve --port 0 --no-content --ready-file "$ROOT/$n/ready.json") \
    >"$ROOT/$n/serve.log" 2>&1 </dev/null &
  wait_for "$n to serve" 15 test -s "$ROOT/$n/ready.json" || return 1
  json 'd["url"]' <"$ROOT/$n/ready.json" >"$ROOT/$n/url"
  local url nonce
  url=$(cat "$ROOT/$n/url")
  nonce=$(bus "$n" open --print | sed -n 's/.*#login=//p')
  # The cookie alone is not enough: the response body carries the session token (C1).
  curl -s -D "$ROOT/$n/headers" -o "$ROOT/$n/session.json" -X POST "$url/api/session" -H "Origin: $url" \
    -H 'Content-Type: application/json' -d "{\"nonce\":\"$nonce\"}"
  sed -n 's/^[Ss]et-[Cc]ookie: \(axon_session=[^;]*\).*/\1/p' "$ROOT/$n/headers" >"$ROOT/$n/cookie"
  json 'd["token"]' <"$ROOT/$n/session.json" >"$ROOT/$n/token"
  test -s "$ROOT/$n/cookie" && test -s "$ROOT/$n/token"
}

# The peer row of instance $1 as JSON, and one field of it.
peer_of() { api "$1" GET /api/fed | json 'd["peers"][0]'; }
peer_field() { peer_of "$1" | json "d[\"$2\"]"; }
peer_is() { [ "$(peer_field "$1" state)" = "$2" ]; }
received_above() { [ "$(received "$1")" -gt "$2" ]; }
received() { api "$1" GET /api/fed | json 'd["peers"][0]["counters"]["received"]'; }

# Sends from instance $1's agent $2 to the remote session of $3's agent $4, kind $5.
# Prints the time (ms) until the receiver's inbox has committed it.
send_timed() {
  local from=$1 agent=$2 to=$3 kind=$5 target before start line
  target=$(remote_target "$1" "$2")
  before=$(received "$to")
  start=$(now_ms)
  line=$(bus "$from" send --from "$agent" --to "$target" --kind "$kind" --body "e2e $kind from $from") || {
    echo "send failed: $line" >&2
    return 1
  }
  case $line in queued*) ;; *) echo "$line" >&2; return 1 ;; esac
  wait_for "$to inbox" 10 received_above "$to" "$before" || return 1
  echo $(($(now_ms) - start))
}
remote_target() { bus "$1" peers --agent "$2" | sed -n 's/^ *\(peer:[^ ]*\).*/\1/p' | head -1; }

# --- the run ---------------------------------------------------------------------------
printf 'axon e2e: %s\n' "$("$AXON" --version 2>&1)"
printf 'workdir : %s (removed on exit)\n\n' "$ROOT"

start_instance a agent-a claude && start_instance b agent-b codex || fail "start two isolated instances" "$(cat "$ROOT"/*/serve.log 2>/dev/null)"
pass "start two isolated instances"

for n in a b; do api $n PUT /api/settings/federation '{"enabled":true}' >/dev/null || fail "turn federation on ($n)"; done
pass "turn federation on in both"

invite=$(api a POST /api/fed/invites '{}' | json 'd["invite"]') || fail "create invite"
api b POST /api/fed/join "{\"invite\":\"$invite\",\"label\":\"alice\"}" >/dev/null || fail "join with the invite"
wait_for "a pending" 10 peer_is a pending_confirm && wait_for "b pending" 10 peer_is b pending_confirm || fail "pair: both sides await confirmation"
pass "pair: both sides await confirmation"
code_a=$(peer_field a pair_code)
code_b=$(peer_field b pair_code)
[ -n "$code_a" ] && [ "$code_a" = "$code_b" ] && pass "pair code matches on both screens ($code_a)" || fail "pair code matches" "a='$code_a' b='$code_b'"
api a POST "/api/fed/peers/$(peer_field a peer_id)/confirm" "{\"pair_code\":\"$code_a\"}" >/dev/null || fail "confirm on a"
api b POST "/api/fed/peers/$(peer_field b peer_id)/confirm" "{\"pair_code\":\"$code_b\"}" >/dev/null || fail "confirm on b"
wait_for "a connected" 15 peer_is a connected && wait_for "b connected" 15 peer_is b connected || fail "pair: both connected"
[ "$(peer_field a path)" = direct ] && pass "pair: connected over a direct path" || fail "pair: direct path" "$(peer_field a path)"

repo_a=$(cat "$ROOT/a/repo-path")
repo_b=$(cat "$ROOT/b/repo-path")
api a POST "/api/fed/peers/$(peer_field a peer_id)/shares" \
  "{\"local_repo\":\"$repo_a\",\"label\":\"project\",\"inbound\":true,\"outbound\":true}" >/dev/null || fail "offer a share"
offered() { api b GET /api/fed | json '[s["share_id"] for s in d["peers"][0]["shares"] if s["state"] == "offered_in"][0]'; }
wait_for "offer on b" 10 offered || fail "share: offer reaches b"
share=$(offered)
api b POST "/api/fed/shares/$share/accept" "{\"local_repo\":\"$repo_b\",\"inbound\":true,\"outbound\":true}" >/dev/null || fail "accept the share"
share_active() { api "$1" GET /api/fed | json 'd["peers"][0]["shares"][0]["state"]' | grep -qx active; }
wait_for "share active a" 10 share_active a && wait_for "share active b" 10 share_active b || fail "share: active on both sides"
pass "share: both owners agreed, both directions on"
has_target() { [ -n "$(remote_target "$1" "$2")" ]; }
wait_for "discovery on a" 15 has_target a agent-a && wait_for "discovery on b" 15 has_target b agent-b || fail "share: each side lists the other's sessions"
pass "share: sessions discovered ($(remote_target a agent-a))"

# Latency: an a->b send until b's inbox has committed it. The receiver rate-limits one
# recipient to 2/s (burst 5), so samples are spaced.
samples=""
for _ in $(seq "$SAMPLES"); do
  ms=$(send_timed a agent-a b agent-b sync) || fail "send a -> b" "$ms"
  samples="$samples $ms"
  sleep 0.6
done
pass "send a -> b ($SAMPLES delivered)"
question_ms=$(send_timed a agent-a b agent-b question) || fail "send a -> b (question)" "$question_ms"
ms=$(send_timed b agent-b a agent-a sync) || fail "send b -> a" "$ms"
samples="$samples $ms"
pass "send b -> a"

db_b=$(find "$ROOT/b/data" -name '*.db' | head -1)
question_id=$(python3 - "$db_b" <<'PY'
import sqlite3, sys
row = sqlite3.connect(sys.argv[1]).execute(
    "SELECT id FROM messages WHERE to_id = 'agent-b' AND needs_reply = 1 ORDER BY rowid DESC LIMIT 1").fetchone()
print(row[0] if row else "")
PY
)
[ -n "$question_id" ] || fail "b received the question"
before=$(received a)
reply=$(bus b reply "$question_id" --from agent-b --body "e2e answer") && case $reply in queued*) ;; *) false ;; esac || fail "reply b -> a" "$reply"
wait_for "answer on a" 10 received_above a "$before" || fail "answer reaches a"
pass "answer: b -> a"

peer_a=$(peer_field a peer_id)
target_a=$(remote_target a agent-a) # the roster stops listing a paused or removed peer
api a POST "/api/fed/peers/$peer_a/pause" >/dev/null || fail "pause"
peer_is a paused && pass "pause: a shows paused" || fail "pause: a shows paused" "$(peer_field a state)"
out=$(bus a send --from agent-a --to "$target_a" --kind sync --body "while paused")
[ "$out" = "refused: peer_paused" ] && pass "pause: sending is refused (peer_paused)" || fail "pause: sending is refused" "$out"

api a POST "/api/fed/peers/$peer_a/resume" >/dev/null || fail "resume"
wait_for "reconnect" 30 peer_is a connected || fail "resume: a connects again" "$(peer_field a state)"
pass "resume: connected again"
wait_for "b connected" 30 peer_is b connected || fail "resume: b sees it connected"
ms=$(send_timed a agent-a b agent-b sync) || fail "resume: a message goes through" "$ms"
pass "resume: a message goes through (${ms} ms)"

api a DELETE "/api/fed/peers/$peer_a" >/dev/null || fail "remove"
peer_is a removed && pass "remove: a shows the peer removed" || fail "remove: state" "$(peer_field a state)"
out=$(bus a send --from agent-a --to "$target_a" --kind sync --body "after removal")
case $out in "refused: "*) pass "remove: sending is refused (${out#refused: })" ;; *) fail "remove: sending is refused" "$out" ;; esac
[ "$(api a GET /api/fed | json 'len([p for p in d["peers"] if p["state"] != "removed"])')" = 0 ] && pass "remove: no live peers left on a" || fail "remove: live peers left"

# --- numbers -----------------------------------------------------------------------------
echo
sorted=$(printf '%s\n' $samples | sort -n)
count=$(printf '%s\n' $sorted | wc -l | tr -d ' ')
p95=$(printf '%s\n' $sorted | sed -n "$(( (count * 95 + 99) / 100 ))p")
p50=$(printf '%s\n' $sorted | sed -n "$(( (count + 1) / 2 ))p")
echo "speed   : send -> remote inbox committed, direct path, $count samples: p50 ${p50} ms, p95 ${p95} ms, max $(printf '%s\n' $sorted | tail -1) ms (target <= ${LATENCY_BUDGET_MS} ms)"
echo "          (includes ~30 ms of polling and python timer overhead; the question took ${question_ms} ms)"
[ "$p95" -le "$LATENCY_BUDGET_MS" ] && pass "speed: p95 within ${LATENCY_BUDGET_MS} ms" || fail "speed: p95 within ${LATENCY_BUDGET_MS} ms" "p95 ${p95} ms"

if [ -z "${FED_E2E_SKIP_SIZE:-}" ]; then
  # What users get is the dist profile (dist-workspace.toml), not the release profile.
  cargo build --profile dist -p axon >/dev/null 2>&1
  if [ -x "$SHIPPED" ]; then
    bytes=$(wc -c <"$SHIPPED" | tr -d ' ')
    printf 'size    : %s is %s bytes (%s MB) of the %s MB budget (decimal)\n' "$SHIPPED" "$bytes" \
      "$(python3 -c "print(f'{$bytes / 1e6:.2f}')")" "$((SIZE_BUDGET / 1000000))"
    [ "$bytes" -le "$SIZE_BUDGET" ] && pass "size: dist binary within the 15 MB budget" || fail "size: dist binary within the 15 MB budget"
  else
    fail "size: dist binary" "cargo build --profile dist -p axon failed"
  fi
fi

echo
[ "$FAILED" = 0 ] && echo "ALL STEPS PASSED"
exit "$FAILED"
