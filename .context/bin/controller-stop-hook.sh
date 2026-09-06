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
# Spawned workers in other herdr panes are not the controller either: once layout.sh
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
changed="$(git status --porcelain --untracked-files=all 2>/dev/null \
  | grep -vE '^.. \.context/' \
  | sed -E 's/^.. //; s/^.* -> //')"
[ -n "$changed" ] || exit 0

# newest change vs the controller's last result (controller lines carry no by=, or by=controller)
now="$(date +%s)"; newest=0
while IFS= read -r f; do
  [ -n "$f" ] || continue
  if [ -e "$f" ]; then m="$(stat -f %m "$f" 2>/dev/null || stat -c %Y "$f" 2>/dev/null || echo "$now")"; else m="$now"; fi
  [ "$m" -gt "$newest" ] && newest="$m"
done <<<"$changed"
last_ts="$(jq -r 'select(.type=="result" and ((.by==null) or (.by=="controller"))) | .ts' "$LOG" 2>/dev/null | tail -1)"
last=0
if [ -n "$last_ts" ]; then
  last="$(date -j -u -f '%Y-%m-%dT%H:%M:%SZ' "$last_ts" +%s 2>/dev/null || date -d "$last_ts" +%s 2>/dev/null || echo 0)"
fi
[ "$newest" -gt "$last" ] || exit 0

list="$(tr '\n' ',' <<<"$changed" | sed 's/,$//')"
reason="Changed files have no result event yet: $list. This repo is log-driven: the commit reactor only commits what a result names. Before you finish, run: append-event.sh result ref=<main file> paths=$list summary=\"<one line>\" (do not git commit; do not ping the reactors). If you did not make some of these changes, still list them or tell the user they are uncommitted."
if command -v jq >/dev/null; then jq -nc --arg r "$reason" '{decision:"block",reason:$r}'
else printf '{"decision":"block","reason":%s}\n' "\"$(sed 's/"/\\"/g' <<<"$reason")\""; fi
exit 0
