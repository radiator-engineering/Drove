# Decisions

Append-only intent lives in `.context/events.jsonl`; this file holds the prose
each `decision` event references by `ref=`.

## log-writers = controller-plus-reactors
Sanctioned writers:
- the **controller** (may omit `by=`, or tag its own operational records
  `by=controller`);
- the **committer** reactor (`by=cursor-committer`: `ack`, `violation`,
  `escalate`, `note`);
- the **doc worker** reactor (`by=doc-worker`: `result` for its doc edits,
  plus its own `ack`/`escalate`).

Breach = any `by=` value outside {`controller`, `cursor-committer`, `doc-worker`}.

Breach check:
`jq -c 'select(.by != null and (.by|IN("controller","cursor-committer","doc-worker")|not))' .context/events.jsonl`

## commit-agent = cursor-commit-reactor + composer-2.5-fast (autonomous)
Commits are landed by an **autonomous reactor**, `.context/bin/cursor-commit-reactor.sh`,
running foregrounded in a dedicated herdr pane under the supervisor
`.context/bin/run-reactor.sh`. It `tail -F`s `.context/events.jsonl` itself —
**no human ping** — and on a completed-work event with a dirty tree invokes
**headless** `cursor-agent -p --force --trust --model composer-2.5-fast` to
author commits grounded in **both** the log (intent) and the diff (the change).

The **reactor**, not the model, records the `ack` (`by=cursor-committer`,
`seq_done`, `origin=<who triggered>`, `outcome=committed|skipped`), so resume is
correct even if the model forgets. Model brief: `.context/handoffs/cursor-committer.md`.

What it does and does not survive:
- Triggers only on controller `result`, `decision key=commit-message`, or
  `result by=doc-worker`. Other decisions never fire a pass. Dirty-gate
  ignores `.context/`.
- First start (no acks of its own) writes a baseline `ack outcome=skipped
  detail="baseline: …"` at the log's current tip and does NOT replay earlier
  events. Later restarts resume from the last real ack.
- Transient failures retry 3× before `ack skipped` + `escalate`.
- Files a commit touched outside the event's `paths=` are recorded as a
  `violation` (detection, not prevention — the commit stands).
- A stale `.git/index.lock` is detected and escalated, never auto-deleted.
- Supervisor respawns on crash, logs each restart as a `note`, gives up on a
  crash loop (`escalate`).
- It does NOT survive the pane, herdr session, or machine going away. After a
  reboot, run `.context/bin/run-reactor.sh` in the pane again.
- AI attribution: `.githooks/commit-msg` strips cursor-agent's auto
  `Co-authored-by: Cursor` trailer (`git config core.hooksPath .githooks` is
  local — re-run on a fresh clone).
- Editor churn files (none) are untracked + ignored so they cannot keep
  the tree permanently dirty.

## doc-agent = doc-sync-reactor + headless Claude (sonnet)
`.context/bin/doc-sync-reactor.sh` watches the log itself. On every
`ack by=cursor-committer outcome=committed` whose `origin != doc-worker` it runs
headless Claude (`claude -p --model sonnet`) loading three skills:
`documentation-writer` (Diátaxis structure; its interactive determinations are
pre-answered and its approval gate waived), `plain-technical-english`
(prose discipline) and `context-engineering` (for `AGENTS.md`, the rules file
every agent loads: the doc worker owns its upkeep — adds durable project-wide
facts a change introduced, deletes stale or task-specific lines, keeps it under
about 120 lines, never edits marker-delimited sections other tools own, never
touches `CLAUDE.md`, which only imports `AGENTS.md`). It edits only
docs,README.md,AGENTS.md, never commits, then reports as
a **real worker** — `result by=doc-worker paths=docs,README.md,AGENTS.md` — which the
committer lands with `origin=doc-worker`, which the doc worker ignores (loop
guard). Brief: `.context/handoffs/doc-worker.md`.

Auth: the reactor runs `claude -p` with `ANTHROPIC_API_KEY` removed from the
environment so headless Claude uses the claude.ai login (`DOC_USE_API_KEY=1`
keeps the key). The prompt is piped on stdin because `--allowedTools` is
variadic and swallows a trailing positional prompt.

## log = OS-protected (append-only)
`.context/events.jsonl` is `chflags uappnd` / `chattr +a` protected when
`setup.sh --protect` was used, so a non-Claude agent cannot rewrite or delete
it; `>>` appends still succeed.
