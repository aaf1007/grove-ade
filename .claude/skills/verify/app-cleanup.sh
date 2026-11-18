#!/bin/sh
# Stops what this run's grove app started, and nothing else. Two steps around grove's own Quit:
#
#   app-cleanup.sh snapshot <run>   while grove runs, and again right before Quit: add every
#                                   process under the app, with its start time, to the saved list
#                                   while it is still the app's. Earlier entries are kept.
#   app-cleanup.sh finish <run>     after Quit and up to 10 s of waiting: force the app only if it
#                                   is still this run's app, stop saved processes that are still
#                                   the same process, and report anything it cannot attribute
#
# Reads <run>/evidence/app.pid, app.started and app.command, saved at launch.
# Exits 0 when every process it can attribute to the run is gone, 1 when cleanup is incomplete.
# It cannot prove that nothing else the run started is left, and says so.
set -u
step=$1
run=$2
evidence=$run/evidence
tree_file=$evidence/app-tree.txt

started() { ps -o lstart= -p "$1" 2>/dev/null | xargs; }
command_of() { ps -o command= -p "$1" 2>/dev/null; }
same() { [ -n "$2" ] && [ "$(started "$1")" = "$2" ]; }
tree() { for child in $(pgrep -P "$1"); do echo "$child"; tree "$child"; done; }
app_pid=$(cat "$evidence/app.pid")
app_started=$(xargs < "$evidence/app.started")
app_command=$(cat "$evidence/app.command")
app_is_ours() { same "$app_pid" "$app_started" && [ "$(command_of "$app_pid")" = "$app_command" ]; }

case $step in
snapshot)
    if ! app_is_ours; then
        echo "pid $app_pid is no longer this run's grove; nothing to snapshot"
        exit 1
    fi
    { [ -f "$tree_file" ] && cat "$tree_file"
      for p in $(tree "$app_pid"); do echo "$p $(started "$p")"; done
    } | sort -u > "$tree_file.new" && mv "$tree_file.new" "$tree_file" || {
        echo "could not save the snapshot to $tree_file; do not Quit until it saves"
        exit 1
    }
    echo "saved list holds $(wc -l < "$tree_file" | xargs) processes under grove pid $app_pid"
    ;;
finish)
    # Without a saved list there is nothing to stop safely, so refuse before signaling anything.
    [ -f "$tree_file" ] || { echo "no snapshot: run 'snapshot' before Quit"; exit 1; }
    incomplete=0
    if app_is_ours; then
        echo "grove pid $app_pid did not quit; killing it (forced quit)"
        kill "$app_pid" 2>/dev/null
    fi
    # The app goes through the same SIGKILL step and final check as its saved processes.
    targets=$evidence/app-targets.txt
    { cat "$tree_file"; echo "$app_pid $app_started"; } > "$targets"
    survivors=0
    while read -r p when; do same "$p" "$when" && { kill "$p" 2>/dev/null; survivors=1; }; done < "$tree_file"
    if [ "$survivors" = 1 ] || app_is_ours; then
        sleep 5
        while read -r p when; do same "$p" "$when" && kill -9 "$p" 2>/dev/null; done < "$targets"
        sleep 1
    fi
    # A killed process can stay listed while it is torn down or until its parent reaps it, so
    # poll up to 5 s, and count a dead one waiting to be reaped (state Z) as gone.
    alive() { same "$1" "$2" && ! ps -o state= -p "$1" 2>/dev/null | grep -q Z; }
    for _ in $(seq 1 25); do
        left=0
        while read -r p when; do alive "$p" "$when" && left=1; done < "$targets"
        [ "$left" = 0 ] && break
        sleep 0.2
    done
    while read -r p when; do
        if alive "$p" "$when"; then echo "STILL RUNNING $p"; incomplete=1; fi
    done < "$targets"
    [ "$survivors" = 1 ] && echo "some saved processes outlived the app and were stopped"
    # Anything else that names the run, or works in a folder under it (as an agent's background
    # job in a worktree does), cannot be proven ours, so it is reported, never killed.
    # lsof reports resolved paths (/private/tmp, not /tmp), so match the resolved home too.
    real_home=$(cd "$run/home" && pwd -P)
    unattributed=$( { pgrep -f "$run/home"; pgrep -f "$real_home"
        lsof -a -u "$(id -u)" -d cwd -Fpn 2>/dev/null | awk -v home="$real_home" '
            /^p/ { pid = substr($0, 2) }
            /^n/ { path = substr($0, 2); if (path == home || index(path, home "/") == 1) print pid }'
    } | sort -un)
    for p in $unattributed; do
        [ "$p" = "$$" ] && continue
        echo "NOT ATTRIBUTED $p $(command_of "$p")"
        incomplete=1
    done
    if [ "$incomplete" = 1 ]; then echo "cleanup incomplete"; exit 1; fi
    echo "attributable cleanup done: every saved process is gone, and nothing else names or works in $run/home."
    echo "Full coverage not verifiable: a process that left grove's tree before every snapshot and does neither cannot be seen."
    ;;
*)
    echo "usage: app-cleanup.sh snapshot|finish <run>"
    exit 2
    ;;
esac
