#!/usr/bin/env bash
# BUS-PLAN §00 and §9 M1: run unsandboxed with a release binary, hyperfine and Python 3.
# Usage: hook-latency.sh [/absolute/path/to/axon-bus]
# Contract decisions: nearest-rank p95 of 500 raw times; empty inbox <=10 ms absolute,
# absent hub <=1 ms above /usr/bin/true p95 measured in the same hyperfine invocation.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "$0")/../../.." && pwd)
binary=${1:-"$repo_root/target/release/axon-bus"}
case "$binary" in /*) ;; *) binary="$PWD/$binary" ;; esac
for tool in hyperfine python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "Required executable missing: $tool" >&2
        exit 2
    fi
done
if [[ ! -x "$binary" ]]; then
    echo "Build a release axon-bus binary first, or pass its absolute path." >&2
    exit 2
fi
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
mkdir -p "$scratch/home" "$scratch/absent" "$scratch/present" "$scratch/config" "$scratch/cache" "$scratch/work"

python3 - "$repo_root/tests/fixtures/hooks/claude/PreToolUse.json" "$scratch" <<'PY'
import json, pathlib, sys
payload = json.loads(pathlib.Path(sys.argv[1]).read_text())
root = pathlib.Path(sys.argv[2])
payload['cwd'] = str(root / 'work')
payload['transcript_path'] = str(root / 'home' / 'missing-transcript.jsonl')
(root / 'hook.json').write_text(json.dumps(payload))
PY

# hyperfine -N launches the binary directly. --input avoids a cat/sh wrapper in timed work.
hook_command=$(python3 - "$binary" <<'PY'
import shlex, sys
print(shlex.join([sys.argv[1], 'hook', 'claude', 'PreToolUse']))
PY
)
isolated() {
    local data_dir=$1
    shift
    env -i PATH="$PATH" HOME="$scratch/home" XDG_DATA_HOME="$data_dir" \
        XDG_CONFIG_HOME="$scratch/config" XDG_CACHE_HOME="$scratch/cache" \
        CODEX_HOME="$scratch/home/.codex" CLAUDE_CONFIG_DIR="$scratch/home/.claude" \
        HERMES_HOME="$scratch/home/.hermes" NO_COLOR=1 TZ=UTC LANG=C "$@"
}
cd -- "$scratch/work"
isolated "$scratch/absent" "$binary" hook claude PreToolUse \
    < "$scratch/hook.json" > "$scratch/absent.stdout" 2> "$scratch/absent.stderr"
if [[ -s "$scratch/absent.stdout" || -s "$scratch/absent.stderr" || -e "$scratch/absent/axon/axon.db" ]]; then
    echo "Absent-hub hook must exit 0 silently without creating a database." >&2
    exit 1
fi
isolated "$scratch/absent" hyperfine -N --warmup 20 --runs 500 \
    --input "$scratch/hook.json" --export-json "$scratch/absent.json" \
    /usr/bin/true "$hook_command"

isolated "$scratch/present" "$binary" init
test -f "$scratch/present/axon/axon.db"
isolated "$scratch/present" "$binary" hook claude PreToolUse \
    < "$scratch/hook.json" > "$scratch/present.stdout"
if [[ -s "$scratch/present.stdout" ]]; then
    echo "An initialized, empty-inbox hook must allow silently." >&2
    exit 1
fi
isolated "$scratch/present" hyperfine -N --warmup 20 --runs 500 \
    --input "$scratch/hook.json" --export-json "$scratch/present.json" "$hook_command"

python3 - "$scratch/absent.json" "$scratch/present.json" <<'PY'
import json, math, sys

def p95(result):
    times = result['times']
    if len(times) != 500 or not all(math.isfinite(t) and t >= 0 for t in times):
        raise SystemExit('Expected 500 finite nonnegative timings per command')
    if any(code != 0 for code in result.get('exit_codes', [])):
        raise SystemExit('A benchmark command failed')
    return sorted(times)[math.ceil(0.95 * len(times)) - 1]

with open(sys.argv[1]) as stream:
    absent = json.load(stream)['results']
with open(sys.argv[2]) as stream:
    present = json.load(stream)['results']
floor, no_hub, empty = p95(absent[0]), p95(absent[1]), p95(present[0])
delta = no_hub - floor
print(f'true p95={floor*1000:.3f} ms; absent p95={no_hub*1000:.3f} ms; delta={delta*1000:.3f} ms')
print(f'empty inbox p95={empty*1000:.3f} ms')
failed = False
if delta > 0.001:
    print('FAIL: absent hub exceeds the spawn floor by more than 1 ms p95', file=sys.stderr)
    failed = True
if empty > 0.010:
    print('FAIL: empty inbox exceeds 10 ms p95', file=sys.stderr)
    failed = True
raise SystemExit(1 if failed else 0)
PY
