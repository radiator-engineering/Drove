#!/usr/bin/env bash
# cursor-commit-reactor.sh — autonomous committer reactor driven by Composer 2.5 Fast.
#
# Watches the append-only coordination log itself (no human ping). On a
# completed-work event with a dirty working tree, it invokes HEADLESS Composer
# 2.5 Fast (`cursor-agent -p`) to author well-structured commits from the two:
# the log (intent) + the diff (the change). The reactor, not the model, records
# the `ack`, so progress is guaranteed even if the model forgets.
#
# Reactor rules (see log-reactors.md), plus the hardening they earned:
#   1. Runs ONCE — a mkdir lock (holding pid) refuses a second instance
#      (exit 3, so a supervisor knows not to respawn).
#   2. Resumes FROM THE LOG — highest `ack ... by=cursor-committer` seq_done.
#      No side cursor file. A FIRST start (no acks of its own) baselines at
#      the log's current tip instead of replaying history it never owned.
#   3. Triggers ONLY on completed work: `result`, or `decision` whose key
#      mentions "commit". Coordination-only decisions never fire a pass.
#   4. Dirty-gate ignores .context/ (coordination edits never trigger).
#   5. Transient Composer failures RETRY (RETRIES x RETRY_SLEEP) before the
#      event is acked skipped — a Cursor blip must not lump later commits.
#   6. After a commit, files outside the event's `paths=` are DETECTED and
#      recorded as a `violation` event (the commit stands; a human decides).
#   7. A timeout that leaves a stale .git/index.lock is detected and reported
#      (never auto-deleted).
#
# Run it FOREGROUNDED in a real terminal (a dedicated herdr pane), ideally via
# the supervisor:  bash .context/bin/run-reactor.sh
# Never `nohup … &` it from an agent's tool-call shell — that shell dies at the
# end of the turn and takes the reactor with it.
#
# Env: EVENTLOG_PATH, MODEL (composer-2.5-fast), PASS_TIMEOUT s (300),
#      RETRIES (3), RETRY_SLEEP s (20).
set -uo pipefail

REPO="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "reactor: not in a git repo" >&2; exit 1; }
cd "$REPO"
# per-repo settings written by setup.sh (shell env still wins: the file uses ${VAR:=default})
[ -f "$REPO/.context/workspace.env" ] && . "$REPO/.context/workspace.env"
LOG="${EVENTLOG_PATH:-$REPO/.context/events.jsonl}"
MODEL="${MODEL:-composer-2.5-fast}"
PASS_TIMEOUT="${PASS_TIMEOUT:-300}"
RETRIES="${RETRIES:-3}"
RETRY_SLEEP="${RETRY_SLEEP:-20}"
ME="cursor-committer"
LOCK="$LOG.$ME.reactor.lock"

command -v jq >/dev/null || { echo "reactor: jq is required" >&2; exit 1; }
command -v cursor-agent >/dev/null || { echo "reactor: cursor-agent not on PATH" >&2; exit 1; }
command -v append-event.sh >/dev/null || { echo "reactor: append-event.sh not on PATH (run safety-check.sh --doctor)" >&2; exit 1; }
[ -e "$LOG" ] || { echo "reactor: no log at $LOG" >&2; exit 1; }
TIMEOUT="$(command -v timeout || command -v gtimeout || true)"

say() { printf 'reactor %s  %s\n' "$(date +%H:%M:%S)" "$*"; }

# --- 1. single instance -----------------------------------------------------
if ! mkdir "$LOCK" 2>/dev/null; then
  other="$(cat "$LOCK/pid" 2>/dev/null || true)"
  if [ -n "$other" ] && kill -0 "$other" 2>/dev/null; then
    say "another $ME reactor is running (pid $other) — not starting"; exit 3
  fi
  say "reclaiming stale lock (pid ${other:-?} is gone)"
  rm -rf "$LOCK"; mkdir "$LOCK" || exit 1
fi
echo "$$" > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

# --- 2. resume point from the log, not a side file --------------------------
acked_through() {
  jq -r --arg me "$ME" 'select(.type=="ack" and .by==$me) | .seq_done // empty' "$LOG" 2>/dev/null \
    | sort -n | tail -1
}

# dirty tree EXCLUDING .context/ (coordination edits alone must not trigger)
dirty_outside_context() {
  git status --porcelain 2>/dev/null | grep -vE '^.. \.context/' | grep -q .
}

# a stale index.lock left by a killed git makes every later commit fail — report it
check_index_lock() {
  [ -e "$REPO/.git/index.lock" ] || return 0
  if ! pgrep -x git >/dev/null 2>&1; then
    say "WARN stale .git/index.lock present with no git running — commits will fail until removed"
    append-event.sh escalate by="$ME" subject="stale .git/index.lock blocks commits" detail="remove it by hand after confirming no git process" >/dev/null
  fi
}

PROMPT_TMPL='You are the committer for this repo (Composer 2.5 Fast), running headless from a log reactor. You are not the controller: the coordination block in AGENTS.md (its "never git commit" and "append a result" rules) addresses the controller, not you. Committing is your job; the reactor records the ack for you.

Read THE TWO and turn finished work into meaningful, well-structured git commits:
1. The coordination log .context/events.jsonl — new events since seq %s tell you WHAT happened and WHY (result/decision/claim, each with a ref= file).
2. The working tree — `git status --porcelain` and `git diff` show the ACTUAL changes.

The triggering event is seq %s.%s

Do this now:
- Group the changed files into coherent commits (one commit = one logical change; split unrelated edits).
- For each: `git add <explicit paths>` (NEVER `git add -A`, NEVER stage anything under .context/), then `git commit` with a concise imperative subject and a body explaining WHY, grounded in the log (cite the driving event seq).
- No AI attribution in messages (no Co-Authored-By, no "Generated with").
- Do NOT write, edit, or append to .context/events.jsonl — the reactor records the outcome itself.
- If the only changes are under .context/, or nothing coherent to commit, make no commit and say so.

Commit the landed work now.'

run_composer() {   # $1=prompt ; returns cursor-agent rc (124 = timeout)
  if [ -n "$TIMEOUT" ]; then
    "$TIMEOUT" "$PASS_TIMEOUT" cursor-agent -p --force --trust --model "$MODEL" --output-format text "$1"
  else
    cursor-agent -p --force --trust --model "$MODEL" --output-format text "$1"
  fi
}

do_pass() {   # $1=trigger seq  $2=resume seq  $3=paths from the event (may be empty)  $4=origin (who triggered)
  local seq="$1" resume="$2" paths="$3" origin="${4:-controller}" scope="" prompt head_before head_after shas rc attempt
  if [ -n "$paths" ]; then
    scope=" It names these files as the task's scope: $paths — stage ONLY those unless the diff makes another file obviously part of the same change."
  fi
  prompt="$(printf "$PROMPT_TMPL" "$resume" "$seq" "$scope")"
  head_before="$(git rev-parse HEAD 2>/dev/null)"
  say "--- Composer pass for seq $seq (resume>$resume) ---"

  rc=1; attempt=0
  while [ "$attempt" -lt "$RETRIES" ]; do
    attempt=$((attempt+1))
    run_composer "$prompt"; rc=$?
    head_after="$(git rev-parse HEAD 2>/dev/null)"
    [ "$head_before" != "$head_after" ] && break          # something landed — done
    [ "$rc" -eq 0 ] && break                               # model ran fine and chose not to commit
    [ "$rc" -eq 124 ] && { check_index_lock; break; }      # timeout: don't hammer, report
    say "composer failed (rc=$rc), attempt $attempt/$RETRIES — retrying in ${RETRY_SLEEP}s"
    sleep "$RETRY_SLEEP"
  done
  head_after="$(git rev-parse HEAD 2>/dev/null)"

  if [ "$head_before" != "$head_after" ]; then
    shas="$(git log --format=%h "$head_before..$head_after" 2>/dev/null | paste -sd, -)"
    append-event.sh ack by="$ME" seq_done="$seq" origin="$origin" outcome=committed ref="$shas" model="$MODEL" >/dev/null
    say "committed $shas -> acked seq $seq"
    # 6. detection: did the commit(s) touch files outside the task's paths=?
    if [ -n "$paths" ]; then
      local changed outside
      changed="$(git diff --name-only "$head_before" "$head_after" 2>/dev/null)"
      # split paths= on commas with tr (no IFS word-splitting: shell-agnostic, glob-safe)
      outside="$(printf '%s\n' "$changed" | while IFS= read -r f; do
        hit=0
        while IFS= read -r p; do
          [ -n "$p" ] || continue
          case "$f" in $p|$p/*) hit=1 ;; esac
        done <<<"$(tr ',' '\n' <<<"$paths")"
        [ "$hit" -eq 0 ] && printf '%s,' "$f"; done)"
      outside="${outside%,}"
      if [ -n "$outside" ]; then
        append-event.sh violation by="$ME" seq_done="$seq" ref="$shas" paths="$outside" detail="commit touched files outside the event's paths=" >/dev/null
        say "VIOLATION seq $seq: commit touched files outside paths= -> $outside"
      fi
    fi
  elif [ "$rc" -eq 124 ]; then
    append-event.sh ack by="$ME" seq_done="$seq" origin="$origin" outcome=skipped detail="composer timed out after ${PASS_TIMEOUT}s; files stay dirty for the next pass" >/dev/null
    say "composer timed out; acked seq $seq (skipped — work remains uncommitted)"
  elif [ "$rc" -ne 0 ]; then
    append-event.sh escalate by="$ME" seq_done="$seq" subject="composer failed $RETRIES times (rc=$rc)" detail="work remains uncommitted; check cursor-agent auth/network" >/dev/null
    append-event.sh ack by="$ME" seq_done="$seq" origin="$origin" outcome=skipped detail="composer failed after $RETRIES attempts (rc=$rc)" >/dev/null
    say "composer failed after $RETRIES attempts; ESCALATED + acked seq $seq (skipped)"
  else
    append-event.sh ack by="$ME" seq_done="$seq" origin="$origin" outcome=skipped detail="model ran, made no commit" >/dev/null
    say "no commit made; acked seq $seq (skipped)"
  fi
}

# --- main loop ---------------------------------------------------------------
resume="$(acked_through)"
if [ -z "$resume" ]; then
  # Cold start with no acks of our own: everything already in the log predates
  # this reactor. Replaying it would commit history that was landed some other
  # way (or reference shas that no longer exist). Baseline at the current tip
  # instead; only events appended from now on trigger a pass.
  resume="$(jq -r '.seq // empty' "$LOG" 2>/dev/null | sort -n | tail -1)"; resume="${resume:-0}"
  append-event.sh ack by="$ME" seq_done="$resume" outcome=skipped detail="baseline: cold start, prior events not replayed" >/dev/null
  say "no prior acks — baselined at seq $resume (prior events not replayed)"
fi
say "model=$MODEL  triggers=[result | decision key=commit-message]  acked through seq $resume"
say "watching $LOG — no ping needed"
check_index_lock

tail -n +1 -F "$LOG" 2>/dev/null | while IFS= read -r line; do
  seq="$(jq -r '.seq // empty' <<<"$line" 2>/dev/null)"; [ -z "$seq" ] && continue
  # 3. trigger only on completed work: controller results/commit decisions, or a
  #    sanctioned reactor's result (doc-worker reports its doc edits this way)
  jq -e '((.by==null) and (.type=="result" or (.type=="decision" and .key=="commit-message")))
         or (.by=="doc-worker" and .type=="result")' \
    <<<"$line" >/dev/null 2>&1 || continue
  done_through="$(acked_through)"; done_through="${done_through:-0}"
  [ "$seq" -le "$done_through" ] && { say "seq $seq already acked (through $done_through), skip"; continue; }
  origin="$(jq -r '.by // "controller"' <<<"$line" 2>/dev/null)"
  if ! dirty_outside_context; then
    append-event.sh ack by="$ME" seq_done="$seq" origin="$origin" outcome=skipped detail="clean tree (nothing outside .context)" >/dev/null
    say "seq $seq trigger but tree clean; acked skipped"; continue
  fi
  paths="$(jq -r '.paths // empty' <<<"$line" 2>/dev/null)"
  do_pass "$seq" "$done_through" "$paths" "$origin"
done
