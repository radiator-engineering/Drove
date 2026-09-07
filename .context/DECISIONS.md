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

## 2026-09-06 Drove v2 pass
- agent-topology = parallel-worktrees: each PR worker gets its own git worktree under ~/Development/Drove-worktrees and its own herdr workspace.
- worker-commits = own-branch: a PR worker commits on its own branch in its own worktree and opens a PR; the commit reactor still owns commits on main.
- design = docs/superpowers/specs/2026-09-06-drove-v2-design.md (D1–D20).
- content-addressed-resources = D21: every IR resource has a digest; ownership tokens drove_name/drove_profile/drove_digest; drift is token comparison first.
- restart-in-place = D22: changed digest restarts the command in the same pane; only topology changes replace.
- readiness-host-side = D23; adopt-no-caller = D24; prompt-file = D25; lint-later = D26.
- radiator-hub-protocol = additions: pane.set_metadata, PaneInfo.metadata + PaneInfo.process, pane.tail, workspace.rename, hub.capabilities (from recon-radiator-gaps-report.md). Drove PR 5 codes against these shapes; hub PR lands them.
- repo-visibility = public (2026-09-06): GitHub Actions minutes are free for public repos.
- pr-review-process = cubic + agent reviewer: every PR gets a cubic review (triggered on open) and a Sonnet reviewer who folds cubic findings in; merge needs APPROVE from the agent reviewer and green CI.
- main-protection = ruleset protect-main (2026-09-06): no deletion, no force push, changes via PR with required checks fmt/clippy/test(ubuntu,macos)/docs/audit; repository admins bypass so the commit reactor's local main commits can be pushed. 0 required approvals because agent reviewers post as the repo owner.
- windows-path-bug: two planner tests fail on Windows (cwd normalized with Unix separators); CI Windows leg dropped in PR 1; fix scheduled after PR 2 (planner v2) lands, since planner.rs is rewritten there.
- pr-lifecycle = .context/PR-WORKFLOW.md: worker owns the PR to done (all threads answered, APPROVE, green, current), reviewer owns the verdict and re-reviews each push, controller merges.
- ambient-env-below-profile = D46: flag > DROVE_* env > profile > file > ambient host env (HERDR_SESSION, RADIATOR_HUB) > built-in; DROVE_SESSION / DROVE_TARGET added as explicit env overrides. Found by running `drove ls` from inside the drove session: monitoring resolved to `drove`.
- down-stops-named-session = D47 (2026-09-07): `drove down` on Herdr, after hooks and detach, runs `herdr session stop NAME` then `herdr session delete NAME` for the resolved named target; never for `default` or an unnamed target. New `HerdrExt::stop_session`, shelled out like `ensure_session`. Spec section 6 of the v4 design.
- prune-stale-state = D48 (2026-09-07): before planning, managed resources whose backend id is absent from the live snapshot are dropped (with their dependents) and planned as creates; `status` reports them as `recreate`; only `up` saves the prune. Fixes issues 24, 20, 22. Spec section 7.
- reuse-root-tab = D49 (2026-09-07): on Herdr, the first declared tab of a workspace Drove itself created is applied onto Herdr's root tab and renamed, so no stray `1` tab remains. Fixes issue 25. Spec section 8.
- down-tolerates-missing = D50 (2026-09-07): `drove down` prunes stale ids against the live snapshot before closing panes and never aborts on a `close_pane` error; missing resources are detached and reported as `pruned` / `close_failed`, exit 0, and the D47 session stop still runs. Fixes issue 29. Spec section 9.
- live-plan = D51 (2026-09-07): `up` re-snapshots and re-plans after it starts the session; a failing per-pane `process_info` never fails the snapshot; the caller pane id must be in the snapshot; `lint` prunes; a corrupt state file is renamed and ignored; a target named `default` means no named session; stop output is scanned line by line. Audit group A. Spec section 10.
- apply-progress = D52 (2026-09-07): apply records ownership and saves per action, collects failures, skips dependents, exits 1 with what remains; `status` shows interrupted journal entries. Audit group B. Spec section 11.
- planner-truth = D53 (2026-09-07): cwd/env edits plan as gated destructive recreate (or `RestartCommand` with cwd for serve panes when Herdr allows); `on_start` edits record only; reorder or split change is a `Conflict`; renames carry only labels. Audit group C. Spec section 12.
- command-drift = D54 (2026-09-07): `plan` compares a serve pane's live command to its declared argv and emits `RestartCommand` with reason `drifted`; `status` shows `drift`. Audit group D. Spec section 12.
- identity-beyond-id = D55 (2026-09-07): managed profiles record their target and resources record label and cwd; a different target starts fresh; `prune_missing` drops a reused id whose live label or cwd contradicts the record; contract test checks field shapes. Audit group E. Spec section 13.

## 2026-09-07 eventlog CLI migration

- reactor-runtime = eventlog-cli-native-react (event 473): the user authorized replacing removed shell helpers with the `eventlog` CLI from the sibling `../event-log` project. Native `eventlog react` owns locking, supervision, intent, resume and acknowledgments; repository scripts supply scoped actions. Contract: `.context/handoffs/eventlog-migration.md`.
- Implementation uses Codex gpt-5.6-terra/high in its own worktree and Herdr workspace; independent review uses gpt-5.6-sol/high in a separate worktree and workspace. Composer 2.5 Fast and Claude Sonnet remain the committer and documentation models.
- The controller owns this user-authorized live cutover. The old reactors were idle and stopped gracefully in their existing maintenance panes; their shutdown handlers released their locks. No locks or log history are manually deleted. Outstanding acknowledgments and documentation work must be reconciled from evidence before any checkpoint is advanced.

## 2026-09-07 reusable infrastructure scope correction

The user clarified that eventlog must set up and maintain infrastructure across
arbitrary repositories and Drovefiles, without repeating the engineering done
here. Reusable reactor actions, lifecycle/setup support, runtime fixes, and
validation belong in `../event-log` and its distributed skill. Drove is a
consumer and migration acceptance case. The three Drove migration worktrees
are archived and unregistered, and their worker workspaces are closed; their
drafts are not accepted or ready for cutover. See `.context/reports/eventlog-handoff.md`.
No new per-project reactor framework should be landed here.

## D56 — Upstream consumer cutover (2026-09-07)

Use eventlog setup and its generated helper, native react, and lifecycle commands. Preserve Composer 2.5 Fast for commit authorship and Claude Sonnet for documentation; model labels alone do not select an invocation. Retire removed shell-helper references and local reactor machinery. Preserve log history and inherited dirty artifacts; recover the audited commit prefix only with reachable Git evidence and exact-path backlog results. Validate recovery on disposable copies before the authorized live cutover. The Drove controller exclusively owns this log; upstream changes belong to the event-log coordinator.
