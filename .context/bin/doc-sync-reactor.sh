#!/usr/bin/env bash
# doc-sync-reactor.sh — autonomous documentation worker driven by headless Claude.
#
# Watches the coordination log itself. When the committer lands a commit
# (`ack by=cursor-committer outcome=committed`), it invokes headless Claude with
# the documentation-writer skill (Diátaxis) to bring docs/ and README.md in line,
# and (when AGENTS.md is a doc root) context-engineering to keep the rules file right-sized,
# with what actually shipped. It then reports through the log as a REAL worker
# (`result by=doc-worker paths=docs,README.md`) so the committer commits the
# doc changes. It never runs `git commit` itself.
#
# Loop safety: the committer stamps every ack with origin=<who triggered it>.
# Acks with origin=doc-worker are this worker's own docs landing — skipped.
#
# Same reactor rules as the committer: single mkdir lock (exit 3 if held),
# resume from own `ack by=doc-worker seq_done`, retries then escalate, run
# FOREGROUNDED in a dedicated herdr pane via run-reactor.sh style supervision.
#
# Env: EVENTLOG_PATH, DOC_MODEL (default sonnet), PASS_TIMEOUT s (600),
#      RETRIES (2), RETRY_SLEEP s (30), DOC_BUDGET_USD (2), DOC_PATHS
#      (default "docs,README.md,AGENTS.md" — comma list of doc roots this
#      worker owns; AGENTS.md enables the context-engineering pass).
set -uo pipefail

REPO="$(git rev-parse --show-toplevel 2>/dev/null)" || { echo "doc-sync: not in a git repo" >&2; exit 1; }
cd "$REPO"
# per-repo settings written by setup.sh (shell env still wins: the file uses ${VAR:=default})
[ -f "$REPO/.context/workspace.env" ] && . "$REPO/.context/workspace.env"
LOG="${EVENTLOG_PATH:-$REPO/.context/events.jsonl}"
DOC_MODEL="${DOC_MODEL:-sonnet}"
PASS_TIMEOUT="${PASS_TIMEOUT:-600}"
RETRIES="${RETRIES:-2}"
RETRY_SLEEP="${RETRY_SLEEP:-30}"
DOC_BUDGET_USD="${DOC_BUDGET_USD:-2}"
DOC_PATHS="${DOC_PATHS:-docs,README.md,AGENTS.md}"
ME="doc-worker"
LOCK="$LOG.$ME.reactor.lock"
BRIEF=".context/handoffs/doc-worker.md"

command -v jq >/dev/null || { echo "doc-sync: jq is required" >&2; exit 1; }
command -v claude >/dev/null || { echo "doc-sync: claude not on PATH" >&2; exit 1; }
command -v append-event.sh >/dev/null || { echo "doc-sync: append-event.sh not on PATH (run safety-check.sh --doctor)" >&2; exit 1; }
[ -e "$LOG" ] || { echo "doc-sync: no log at $LOG" >&2; exit 1; }
TIMEOUT="$(command -v timeout || command -v gtimeout || true)"

say() { printf 'doc-sync %s  %s\n' "$(date +%H:%M:%S)" "$*"; }

# --- single instance ----------------------------------------------------------
if ! mkdir "$LOCK" 2>/dev/null; then
  other="$(cat "$LOCK/pid" 2>/dev/null || true)"
  if [ -n "$other" ] && kill -0 "$other" 2>/dev/null; then
    say "another $ME reactor is running (pid $other) — not starting"; exit 3
  fi
  say "reclaiming stale lock (pid ${other:-?} is gone)"; rm -rf "$LOCK"; mkdir "$LOCK" || exit 1
fi
echo "$$" > "$LOCK/pid"
trap 'rm -rf "$LOCK"' EXIT

acked_through() {
  jq -r --arg me "$ME" 'select(.type=="ack" and .by==$me) | .seq_done // empty' "$LOG" 2>/dev/null | sort -n | tail -1
}

# files currently dirty under the doc roots (sorted, one per line).
# -z keeps paths with spaces intact (git text-porcelain C-quotes them, and
# `awk '{print $NF}'` would then return a fragment of the quoted path). -z
# records are NUL-terminated, not newline-terminated, and for a rename/copy
# (status R/C) the destination record is followed by a SECOND, unprefixed
# record holding the source path — piping through `tr '\0' '\n'` would strip
# the same 3-char status prefix from that source record too, corrupting it.
# Read records directly and consume the source record explicitly instead.
dirty_docs() {
  git status --porcelain -z 2>/dev/null | {
    while IFS= read -r -d '' entry; do
      st="${entry:0:2}" f="${entry:3}"
      case "$st" in R*|C*) IFS= read -r -d '' _ || true ;; esac  # discard source path
      [ -n "$f" ] || continue
      while IFS= read -r p; do [ -n "$p" ] || continue; case "$f" in "$p"|"$p"/*) echo "$f"; break ;; esac; done <<<"$(tr ',' '\n' <<<"$DOC_PATHS")"
    done
  } | sort -u
}

# The documentation-writer skill is interactive by design (it asks four
# clarifying questions and awaits outline approval). No human is present here,
# so the four determinations are answered up front and the approval gate is
# waived; only its Diátaxis structure and principles are kept.
PROMPT_TMPL='Load and follow TWO skills for this task: `documentation-writer` (Diátaxis structure) and `plain-technical-english` (prose discipline: short sentences, one idea each, active voice, no filler or stock phrases, one term per thing; run its final gate on everything you write). Keep this project'"'"'s documentation in sync with a change that just shipped. You are the DOC WORKER, running HEADLESS from a log reactor: no human will answer questions. You are not the controller: the coordination block in AGENTS.md addresses the controller, not you. You never run append-event.sh and never append to the log; the reactor reports your work as a result event when you finish. Just edit the files and stop. documentation-writer'"'"'s four required determinations are answered here — do NOT ask them, and do NOT wait for outline approval; propose the structure to yourself and write the content in this one pass.

- Document type: whichever Diátaxis quadrant(s) the shipped change actually affects (tutorial / how-to / reference / explanation). Put each piece in the right quadrant; do not force all four.
- Target audience: a developer new to this repository.
- User goal: understand and correctly use what this change added or altered.
- Scope: ONLY what the commit(s) below changed. Do not document unrelated parts of the project.

What shipped (the two sources of truth):
1. Commit(s): %s — inspect with `git show --stat` and `git show` for the diff.
2. The log event that drove them (seq %s), and the coordination log .context/events.jsonl for surrounding intent (result/decision events, their ref= files).

Where docs live: `docs/` organized as docs/tutorials, docs/how-to, docs/reference, docs/explanation, plus `README.md` as the entry point/index. Read the existing docs first and match their tone and terminology. Create the structure if it does not exist yet.

AGENTS.md upkeep (only if `AGENTS.md` is among the editable paths below): after the docs, load the `context-engineering` skill and treat `AGENTS.md` as the project'"'"'s rules file, the one every agent loads on every task. Compare it with what shipped and fix it in the same pass: add a durable project-wide fact the change introduced (a command, a convention, a boundary), correct or delete any line the diff made stale, and delete anything task-specific or already obvious from the code. Keep the context-engineering shape: what the project is, stack, commands, conventions, boundaries, at most one short pattern example. Keep the whole file under about 120 lines; shorter is better, a rules file is an attention budget. Never edit inside a marker-delimited section (between `<!-- name:start -->` and `<!-- name:end -->`, or `<!-- name -->` and `<!-- /name -->`): those belong to other tools. Never touch `CLAUDE.md`; it is an import stub for `AGENTS.md`.

Rules:
- Edit ONLY files under: %s. Never touch anything else.
- Never write, edit, or append to .context/events.jsonl; never run git commit, git add, or any git command that changes state. The reactor reports your work and a separate committer lands it.
- If the change genuinely needs no documentation (pure internal refactor, coordination-only), change nothing and say so plainly.
- Accuracy over volume: every command, path, and behavior you document must match the diff.'

do_pass() {   # $1=trigger seq  $2=shas  $3=driving seq
  local seq="$1" shas="$2" drive="$3" prompt before after changed rc attempt
  prompt="$(printf "$PROMPT_TMPL" "$shas" "$drive" "$DOC_PATHS")"
  before="$(dirty_docs)"
  say "--- Doc pass for ack seq $seq (commits $shas) ---"
  rc=1; attempt=0
  while [ "$attempt" -lt "$RETRIES" ]; do
    attempt=$((attempt+1))
    # The prompt goes in on STDIN, never as a positional argument:
    # `--allowedTools <tools...>` is variadic and would swallow it as a tool name
    # (claude then fails with "Input must be provided either through stdin...").
    # Auth: a stray ANTHROPIC_API_KEY in the shell env takes precedence over
    # the claude.ai login and bills that key's account ("Credit balance is too
    # low" when it is empty). Drop it unless DOC_USE_API_KEY=1 asks for it.
    # LOG_DRIVEN_WORKER tells the repo's Stop hook this is a worker, not the controller
    local -a auth=(env); [ -n "${DOC_USE_API_KEY:-}" ] || auth+=(-u ANTHROPIC_API_KEY); auth+=("LOG_DRIVEN_WORKER=$ME")   # -u must precede any NAME=VALUE: env stops option parsing there
    printf '%s' "$prompt" | ${TIMEOUT:+"$TIMEOUT" "$PASS_TIMEOUT"} ${auth[@]+"${auth[@]}"} claude -p \
      --model "$DOC_MODEL" --permission-mode acceptEdits --max-budget-usd "$DOC_BUDGET_USD" \
      --allowedTools "Skill,Read,Glob,Grep,Edit,Write,Bash(git show:*),Bash(git log:*),Bash(git diff:*),Bash(git status:*),Bash(ls:*),Bash(cat:*),Bash(jq:*)"
    rc=$?
    after="$(dirty_docs)"
    changed="$(comm -13 <(printf '%s\n' "$before") <(printf '%s\n' "$after") | grep -v '^$' || true)"
    [ -n "$changed" ] && break
    [ "$rc" -eq 0 ] && break
    [ "$rc" -eq 124 ] && break
    say "claude failed (rc=$rc), attempt $attempt/$RETRIES — retrying in ${RETRY_SLEEP}s"; sleep "$RETRY_SLEEP"
  done

  if [ -n "$changed" ]; then
    local n; n="$(printf '%s\n' "$changed" | grep -c .)"
    # report as a REAL worker: the committer picks this result up and commits it
    append-event.sh result by="$ME" agent="$ME" ref="$BRIEF" paths="$DOC_PATHS" \
      summary="docs synced to $shas ($n file(s))" for_ack="$seq" >/dev/null
    append-event.sh ack by="$ME" seq_done="$seq" outcome=updated files="$n" ref="$shas" model="$DOC_MODEL" >/dev/null
    say "updated $n doc file(s) for $shas -> result reported, acked seq $seq"
  elif [ "$rc" -eq 124 ]; then
    append-event.sh ack by="$ME" seq_done="$seq" outcome=skipped detail="claude timed out after ${PASS_TIMEOUT}s" >/dev/null
    say "timed out; acked seq $seq (skipped)"
  elif [ "$rc" -ne 0 ]; then
    append-event.sh escalate by="$ME" seq_done="$seq" subject="doc sync failed $RETRIES times (rc=$rc)" detail="check claude auth/network" >/dev/null
    append-event.sh ack by="$ME" seq_done="$seq" outcome=skipped detail="claude failed after $RETRIES attempts (rc=$rc)" >/dev/null
    say "failed after $RETRIES attempts; ESCALATED + acked seq $seq (skipped)"
  else
    append-event.sh ack by="$ME" seq_done="$seq" outcome=skipped detail="no documentation change needed" >/dev/null
    say "no doc change needed; acked seq $seq (skipped)"
  fi
}

# --- main loop ------------------------------------------------------------------
resume="$(acked_through)"
if [ -z "$resume" ]; then
  # Cold start with no acks of our own: the committer acks already in the log
  # predate this worker and may name amended/rewritten shas. Baseline at the
  # current tip; only commits landed from now on get a doc pass.
  resume="$(jq -r '.seq // empty' "$LOG" 2>/dev/null | sort -n | tail -1)"; resume="${resume:-0}"
  append-event.sh ack by="$ME" seq_done="$resume" outcome=skipped detail="baseline: cold start, prior events not replayed" >/dev/null
  say "no prior acks — baselined at seq $resume (prior events not replayed)"
fi
say "model=$DOC_MODEL  doc roots=[$DOC_PATHS]  trigger=[ack by=cursor-committer outcome=committed, origin!=doc-worker]  acked through seq $resume"
say "watching $LOG — no ping needed"

tail -n +1 -F "$LOG" 2>/dev/null | while IFS= read -r line; do
  seq="$(jq -r '.seq // empty' <<<"$line" 2>/dev/null)"; [ -z "$seq" ] && continue
  jq -e '.type=="ack" and .by=="cursor-committer" and .outcome=="committed" and ((.origin // "controller") != "doc-worker")' \
    <<<"$line" >/dev/null 2>&1 || continue
  done_through="$(acked_through)"; done_through="${done_through:-0}"
  [ "$seq" -le "$done_through" ] && { say "seq $seq already acked (through $done_through), skip"; continue; }
  shas="$(jq -r '.ref // empty' <<<"$line")"; drive="$(jq -r '.seq_done // empty' <<<"$line")"
  [ -z "$shas" ] && { append-event.sh ack by="$ME" seq_done="$seq" outcome=skipped detail="ack carried no commit ref" >/dev/null; continue; }
  do_pass "$seq" "$shas" "$drive"
done
