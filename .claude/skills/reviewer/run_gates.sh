#!/bin/sh
# Runs the six gates, plus the checks CI ran besides them, on one commit in a clean, detached
# checkout, and prints PASS or FAIL for each.
# The gate list lives here, not in the commit under test. The reviewer runs the copy on main.
# Usage, from any checkout of grove: run_gates.sh <commit>. Fetch the commit first.
# Exits 0 only when every gate passes. Logs stay in the folder it prints.

set -u
[ $# -eq 1 ] || { echo "usage: run_gates.sh <commit>" >&2; exit 2; }

repo_root=$(git rev-parse --show-toplevel) || exit 2
commit=$(git rev-parse --verify --quiet "$1^{commit}") || { echo "run_gates: unknown commit $1" >&2; exit 2; }
# .githooks/pre-push takes the same lock, so only one gate run goes at a time on this Mac. Runs
# that overlap overload it until timing tests fail. The lock is a flock on fd 9, which the kernel drops when
# this shell exits, however it dies. The gates run with fd 9 closed, so a test's stray child
# process cannot hold the lock forever. This copy of the lock code
# must match the hook's: the reviewer runs this file alone, fetched from main.
gate_lock_file="$HOME/.cache/grove-gates.lock"
mkdir -p "$HOME/.cache" || exit 2
exec 9>> "$gate_lock_file"
python3 - "$gate_lock_file" "run_gates.sh on $commit (pid $$)" <<'TAKE_GATE_LOCK' || exit 2
import fcntl, os, sys, time
gate_lock_file, owner = sys.argv[1:]
try:
    fcntl.flock(9, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    holder = open(gate_lock_file).read().strip() or "another gate run"
    print(f"run_gates: waiting for the gate lock, held by {holder}", file=sys.stderr, flush=True)
    fcntl.flock(9, fcntl.LOCK_EX)
os.ftruncate(9, 0)
os.write(9, f"{owner} since {time.strftime('%H:%M:%S')}\n".encode())
TAKE_GATE_LOCK

temp_root=${TMPDIR:-/tmp}
run_folder=$(mktemp -d "${temp_root%/}/grove-reviewer.XXXXXX")
checkout="$run_folder/checkout"
logs="$run_folder/logs"
mkdir -p "$logs"

git -C "$repo_root" worktree add --detach --quiet "$checkout" "$commit" || exit 2
trap 'git -C "$repo_root" worktree remove --force "$checkout"' EXIT

# Compiled dependencies are kept between runs to save time. The crates of the checkout always
# build fresh, because their path differs on every run.
export CARGO_TARGET_DIR="${GROVE_REVIEWER_TARGET_DIR:-$HOME/.cache/grove-reviewer/target}"

failed=0
run_gate() {
  gate_folder=$1
  shift
  gate_name="$* (in $gate_folder)"
  log="$logs/$(echo "$gate_name" | tr -c 'a-zA-Z0-9' '_').log"
  if (cd "$checkout/$gate_folder" && "$@") > "$log" 2>&1 9>&-; then
    echo "PASS  $gate_name"
  else
    echo "FAIL  $gate_name  log: $log"
    failed=1
  fi
}

echo "Gates on $commit"
run_gate . cargo fmt --check
run_gate . cargo clippy --all-targets -- -D warnings
run_gate . cargo test

# The checks the CI workflow ran besides the six gates. load-session needs pv.
run_gate . python3 -m unittest discover .github/scripts
run_gate . python3 fixtures/diff-repo/check.py
run_gate . python3 -m unittest discover fixtures/fake-agents
run_gate . python3 fixtures/chat-session/check.py
run_gate . sh .claude/hooks/block-merge.test.sh
run_gate . sh scripts/public_snapshot.test.sh
run_gate . python3 fixtures/load-session/check.py

for typescript_folder in app sidecar/claude; do
  if (cd "$checkout/$typescript_folder" && npm ci --prefer-offline --no-audit --no-fund) > "$logs/npm_ci_${typescript_folder%%/*}.log" 2>&1 9>&-; then
    run_gate "$typescript_folder" npx --no-install tsc --noEmit
    run_gate "$typescript_folder" npx --no-install eslint .
    run_gate "$typescript_folder" npx --no-install vitest run
  else
    echo "FAIL  npm ci (in $typescript_folder), so tsc, eslint and vitest did not run  log: $logs/npm_ci_${typescript_folder%%/*}.log"
    failed=1
  fi
done

[ "$failed" -eq 0 ] && echo "All gates passed." || echo "At least one gate failed."
exit "$failed"
