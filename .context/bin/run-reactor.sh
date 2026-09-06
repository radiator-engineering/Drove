#!/usr/bin/env bash
# run-reactor.sh — supervisor for any log reactor.
#
# Keeps a reactor alive across crashes: if it exits for any reason other than
# "another instance already holds the lock" (exit 3) or a clean Ctrl+C, it is
# respawned after a short pause, and the restart is recorded in the log so the
# gap is auditable. Run this FOREGROUNDED in the reactor's herdr pane:
#   bash .context/bin/run-reactor.sh                        # committer (default)
#   bash .context/bin/run-reactor.sh doc-sync-reactor.sh    # doc worker
# The argument is a script name in this directory or an absolute path.
#
# What it cannot survive: the pane, herdr session, or machine going away. After
# a reboot, run the line above again — the reactor resumes from its last ack.
set -uo pipefail
set -m   # job control: a backgrounded child gets its own process group even without setsid
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REACTOR="${1:-cursor-commit-reactor.sh}"
case "$REACTOR" in /*) ;; *) REACTOR="$HERE/$REACTOR" ;; esac
[ -f "$REACTOR" ] || { echo "supervisor: no such reactor: $REACTOR" >&2; exit 2; }
NAME="$(basename "$REACTOR" .sh)"
# the log writer name this reactor is sanctioned under (see DECISIONS.md)
case "$NAME" in
  cursor-commit-reactor) BY=cursor-committer ;;
  doc-sync-reactor)      BY=doc-worker ;;
  *)                     BY="$NAME" ;;
esac
PAUSE="${SUPERVISOR_PAUSE:-5}"
MAX_RAPID="${SUPERVISOR_MAX_RAPID:-5}"   # this many crashes inside RAPID_WINDOW s => stop, something is wrong
RAPID_WINDOW=120

rapid=0; window_start=$(date +%s)
child=""
stop() {
  echo "supervisor: stopped by user"
  # `set -m` above puts the backgrounded reactor in its own process group
  # (pgid == its pid), so signaling that group also reaches any cursor-agent
  # or claude child it spawned, not just the reactor shell itself.
  [ -n "$child" ] && kill -TERM -- "-$child" 2>/dev/null
  exit 0
}
trap stop INT TERM

while :; do
  bash "$REACTOR" & child=$!
  wait "$child"; rc=$?; child=""
  case "$rc" in
    0)   echo "supervisor: reactor exited cleanly"; exit 0 ;;
    3)   echo "supervisor: another reactor holds the lock — not respawning"; exit 3 ;;
    130) echo "supervisor: reactor interrupted"; exit 0 ;;
  esac
  now=$(date +%s)
  if [ $((now - window_start)) -gt "$RAPID_WINDOW" ]; then rapid=0; window_start=$now; fi
  rapid=$((rapid+1))
  if [ "$rapid" -ge "$MAX_RAPID" ]; then
    echo "supervisor: reactor crashed $rapid times in ${RAPID_WINDOW}s (last rc=$rc) — giving up; fix the cause and rerun"
    command -v append-event.sh >/dev/null && \
      append-event.sh escalate by="$BY" subject="$NAME crash loop" detail="$rapid crashes in ${RAPID_WINDOW}s, last rc=$rc; supervisor stopped" >/dev/null
    exit 1
  fi
  echo "supervisor: reactor died (rc=$rc) — restarting in ${PAUSE}s (crash $rapid/$MAX_RAPID in window)"
  command -v append-event.sh >/dev/null && \
    append-event.sh note by="$BY" detail="$NAME died rc=$rc; supervisor restarting" >/dev/null
  sleep "$PAUSE"
done
