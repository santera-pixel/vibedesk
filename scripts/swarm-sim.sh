#!/usr/bin/env bash
# Runs the headless swarm against the scripted agent double (src/bin/swarm-sim.rs)
# in a freshly seeded workspace, and prints how long the run took.
#
#   VIBEDESK_SIM_CAPS=/path/to/capabilities.json scripts/swarm-sim.sh [goal]
#
# The workspace is rebuilt every time on purpose. The double answers each member
# from a fixed script, and the engine refuses a plan whose acceptance already
# passes on the base commit — which is what the previous run's merged work
# produces. A shared workspace turns the second run into a planning stall.
#
# The data directory is separate from the app's so a sim run cannot touch real
# run history. It needs a capabilities.json; the script finds one in the app's own
# data directories, or takes one from VIBEDESK_SIM_CAPS. It never invents one: a
# run that passes because a capability was assumed is not a test.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
root="${VIBEDESK_SIM_DIR:-/c/Users/Santera/AppData/Local/Temp/vd-swarm-sim}"
ws="$root/ws"
data="$root/data"
goal="${1:-Raise a and b to 2, each with its own test.}"
script="$repo/src-tauri/tests/fixtures/swarm-sim-script.json"

mkdir -p "$data"
if [ ! -f "$data/capabilities.json" ]; then
    for candidate in "${VIBEDESK_SIM_CAPS:-}" \
        "$repo/src-tauri/target/debug/data/capabilities.json" \
        "$APPDATA/VibeDesk/capabilities.json" \
        "$HOME/.config/vibedesk/capabilities.json"; do
        if [ -n "$candidate" ] && [ -f "$candidate" ]; then
            cp "$candidate" "$data/capabilities.json"
            echo "capabilities from $candidate"
            break
        fi
    done
fi
if [ ! -f "$data/capabilities.json" ]; then
    echo "no capabilities.json found — set VIBEDESK_SIM_CAPS to your VibeDesk one" >&2
    exit 2
fi

rm -rf "$ws"
mkdir -p "$ws/src" "$ws/test"
printf 'export const a = 0;\n' > "$ws/src/a.js"
printf 'export const b = 0;\n' > "$ws/src/b.js"
printf '// placeholder\n' > "$ws/test/README.md"
git -C "$ws" init -q
git -C "$ws" add -A
git -C "$ws" -c user.email=sim@local -c user.name=sim commit -qm "seed sources"

marker="$root/.started"
: > "$marker"
sleep 1

start=$(date +%s)
set +e
# Windows-form paths: the runner resolves VIBEDESK_ACP_BIN with `Path::is_file`,
# and a forward-slash absolute path was reported as "not a file" from one working
# directory while the same file resolved from another.
VIBEDESK_ACP_BIN="$(cygpath -w "$repo/src-tauri/target/debug/swarm-sim.exe")" \
VIBEDESK_SIM_SCRIPT="$(cygpath -w "$script")" \
VIBEDESK_DATA_DIR="$(cygpath -w "$data")" \
  "$repo/src-tauri/target/debug/swarm-run.exe" \
  --workspace "$ws" \
  --goal "$goal" \
  --model deepseek \
  --budget-micros 1500000 \
  --concurrency 2 \
  --timeout "${VIBEDESK_SIM_TIMEOUT:-220}"
status=$?
set -e
echo "exit=$status secs=$(( $(date +%s) - start ))"

leftover=$(find "$HOME/.vibe/worktrees" -mindepth 1 -maxdepth 1 -type d \
    -newer "$marker" ! -name '.claims' 2>/dev/null | wc -l)
alive=$(tasklist 2>/dev/null | grep -ci 'swarm-sim.exe' || true)
echo "worktrees_left=$leftover sim_processes=$alive"
exit "$status"
