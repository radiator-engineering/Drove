#!/usr/bin/env bash
# controller-stop-hook.sh — Claude Code Stop hook for a log-driven repo.
# If files outside .context/ changed after the controller's last `result` event,
# refuse the stop once and hand the file list back so the controller appends
# the event. Never blocks twice in a row (stop_hook_active), so it cannot loop.
# Installed by setup-log-driven-workspace/setup.sh into .claude/settings.json.
set -u
# Reactor workers (headless claude -p launched by a reactor) are not the controller:
# the reactor reports for them. They carry LOG_DRIVEN_WORKER=<name> in their env.
[ -n "${LOG_DRIVEN_WORKER:-}" ] && exit 0
payload="$(cat 2>/dev/null || true)"
# Spawned workers in other herdr panes are not the controller either: once the workspace setup
# has recorded the controller's pane, only that pane is held to the result rule.
REPO0="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null)}"
if [ -n "${HERDR_PANE_ID:-}" ] && [ -f "$REPO0/.context/layout.json" ] && command -v jq >/dev/null; then
  ctl="$(jq -r '.controller.pane // empty' "$REPO0/.context/layout.json" 2>/dev/null)"
  [ -n "$ctl" ] && [ "$ctl" != "$HERDR_PANE_ID" ] && exit 0
fi
if command -v jq >/dev/null && [ -n "$payload" ]; then
  [ "$(jq -r '.stop_hook_active // false' <<<"$payload")" = true ] && exit 0
fi
REPO="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null)}"; [ -n "$REPO" ] || exit 0
cd "$REPO" || exit 0
LOG=.context/events.jsonl; [ -e "$LOG" ] || exit 0

# changed paths outside .context/ (renames: keep the new name). Everything else,
# AGENTS.md and .claude/ included, must be named in a result to get committed.
# -z keeps raw path names intact (text porcelain C-quotes paths with
# whitespace, breaking both the stat lookup below and the paths= list this
# hook suggests). -z records are NUL-terminated; a rename/copy (status R/C)
# emits the destination record followed by a second, unprefixed record
# holding the source path, which must be consumed and dropped, not treated
# as another changed file.
changed="$(
  git status --porcelain -z --untracked-files=all 2>/dev/null | {
    while IFS= read -r -d '' entry; do
      st="${entry:0:2}" f="${entry:3}"
      case "$st" in R*|C*) IFS= read -r -d '' _ || true ;; esac  # discard source path
      case "$f" in .context/*) continue ;; esac
      printf '%s\n' "$f"
    done
  }
)"
[ -n "$changed" ] || exit 0

# newest change vs the controller's last result (controller lines carry no by=, or by=controller)
# Compare at nanosecond resolution: whole-second timestamps let a write land in
# the same second as the result and compare equal, silently passing the gate.
# file mtime in nanoseconds (BSD stat gives "<secs>.<nsecs>"; GNU stat's %.9Y matches)
mtime_ns() {
  local s
  s="$(stat -f '%Fm' "$1" 2>/dev/null || stat -c '%.9Y' "$1" 2>/dev/null)" || { echo 0; return; }
  printf '%s\n' "${s/./}"
}
# `date +%s%N` is GNU; older BSD/macOS date has no %N and emits the literal
# "N" suffix (non-numeric), so validate before trusting it and fall back to
# whole seconds (padded to ns) when it isn't purely digits.
now="$(date +%s%N 2>/dev/null || true)"
case "$now" in ''|*[!0-9]*) now="$(date +%s 2>/dev/null || echo 0)000000000" ;; esac
newest=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  if [ -e "$f" ]; then m="$(mtime_ns "$f")"; else m="$now"; fi
  [ -n "$m" ] || m="$now"
  [ "$m" -gt "$newest" ] && newest="$m"
done <<<"$changed"
last_line="$(tail -1 "$LOG" 2>/dev/null)"
last=0
if jq -e 'select(.type=="result" and ((.by==null) or (.by=="controller")))' <<<"$last_line" >/dev/null 2>&1; then
  # eventlog append's ts field is second-resolution only, so when the matching
  # result is the log's own last line, use the LOG FILE's mtime instead: the
  # append (a single atomic >>) stamps it with real nanosecond resolution, so
  # a later write to any other file compares strictly greater, collision-free.
  last="$(mtime_ns "$LOG")"
else
  # the matching result isn't the newest line (something else was appended
  # after it) — fall back to its second-resolution ts; same-second writes are
  # still ambiguous here, but this is a narrow, uncommon path.
  last_ts="$(jq -r 'select(.type=="result" and ((.by==null) or (.by=="controller"))) | .ts' "$LOG" 2>/dev/null | tail -1)"
  if [ -n "$last_ts" ]; then
    last_s="$(date -j -u -f '%Y-%m-%dT%H:%M:%SZ' "$last_ts" +%s 2>/dev/null || date -d "$last_ts" +%s 2>/dev/null || echo 0)"
    last="${last_s}999999999"
  fi
fi
[ "$newest" -gt "$last" ] || exit 0

list="$(tr '\n' ',' <<<"$changed" | sed 's/,$//')"
reason="Changed files have no result event yet: $list. This repo is log-driven: the commit reactor only commits what a result names. Before you finish, run: eventlog append result ref=<main file> paths=\"$list\" summary=\"<one line>\" (do not git commit; do not ping the reactors). If you did not make some of these changes, still list them or tell the user they are uncommitted."
if command -v jq >/dev/null; then jq -nc --arg r "$reason" '{decision:"block",reason:$r}'
else printf '{"decision":"block","reason":%s}\n' "\"$(sed 's/"/\\"/g' <<<"$reason")\""; fi
exit 0
