> Archived 2026-09-07: this assignment is superseded. The worker workspace and worktree are closed. See `.context/reports/eventlog-handoff.md` for the archive and upstream scope. Do not resume this brief.

# Worker: eventlog-migration — Terra

## Current assignment override (event 504, user-selected Luna)

Your runtime draft was reported but not accepted and is reassigned to `eventlog-runtime` (Terra/high). You acknowledged stopping runtime edits. Your live claim is event 506. Finish only wiring/current docs/fixtures/guard/gitignore; then import the reviewed runtime commit when the controller provides it and own the combined PR. Restore your old `.context/bin` drafts to HEAD before importing (the controller preserved the draft); discard unused generated `.context/eventlog.toml`. Never modify Terra's runtime logic yourself without coordinating.

Frozen integration: reactor serve is `["bash", ".context/bin/run-reactor.sh", script]`; lifecycle on_start is `["bash", ".context/bin/reactor-lifecycle.sh", "start", agent, model, slug]`; on_stop is `["bash", ".context/bin/reactor-lifecycle.sh", "stop", agent]`. Terra owns those scripts and `.context/workspace.env`, Makefile and CI. Keep lifecycle assertions in example tests. Update ALL active legacy command references in AGENTS.md (preserving markers), .context/EVENTLOG.md, reactor handoff briefs, Drovefile/examples/current docs; historical design docs can remain historical. Add `.worktrees/` ignore. Add operational runbook docs/eventlog-reactors.md, with placeholders explicitly coordinated with Terra until its implementation is ready. Preserve the local Claude eventlog guard. Do not leave this task at acknowledgment: complete these narrowed changes, test formatting/DSL examples, make a scoped wiring commit, then wait for the runtime commit and integrate it into one PR. The rest of this original brief supplies context and combined completion requirements; the narrower override wins for edits.

You were spawned by the Drove controller. Implement the eventlog CLI migration in your isolated worktree `/Users/jjmartin/Development/Drove/.worktrees/eventlog-migration`, branch `fix/eventlog-reactors`. The user explicitly authorized this migration and Herdr workers. Use gpt-5.6-terra with high reasoning. Do not spawn subagents. Do not append to the log. Never operate live Herdr panes or reactors; the controller owns cutover.

You MAY commit only on your own branch, push it, and open a PR. Never commit on main and never merge. Read `/Users/jjmartin/Development/Drove/.context/PR-WORKFLOW.md` and follow the worker polling/review loop through PR DONE. The independent Sol reviewer is named `eventlog-review`; the controller will route findings. All substantive reports must be visible in your Herdr response. Do not depend on collaboration tools to reach the controller.

## Contract (frozen before delegation)

Use the installed `eventlog` CLI, backed by `/Users/jjmartin/Development/event-log` (read-only reference), as the sole log writer and reactor runtime. `eventlog react` owns locking, resume, supervision, intent, authorization, outcome and ack. Preserve cursor-committer (Composer 2.5 Fast) and doc-worker (Claude Sonnet), scoped commits, doc-loop prevention, timeouts, auth behavior, and on-demand Drove semantics. Read the event-log source, docs/reference/react-command.md, reactor action/loop/voter references, and CLI help. Do not rebuild or edit the sibling repo.

Replace the custom tail/lock/supervisor loops with thin runtime launchers and tested one-pass actions. Never delete reactor locks from shell. Validate event fields against `eventlog vocab`; use `eventlog append --as NAME TYPE ...`, not `by=`. Avoid custom vocabulary if origin can be recovered from the driving event; if config is necessary declare it explicitly. Work must not replay old completed commits or suppress failed pending work. Keep cold-start/cutover behavior explicit: controller repairs live state; scripts must not silently baseline away work.

Update active repo wiring, examples, coordination instructions and current docs from removed append-event.sh/eventlog-view.sh/check-claims.sh to eventlog commands. Keep AGENTS.md managed markers and preserve its boundary rules, updating the content for this user-authorized migration. Stop-hook guidance must emit a valid result (including agent lifecycle requirements). Preserve historical design documents unless changing one is truly needed. Install/retain the local Claude eventlog guard without broadening permission settings.

Only the event-scoped paths may be committed. Pre-existing staged changes must not be swept into an unrelated commit. Doc changes outside DOC_PATHS are violations; doc results must identify actual changed paths and avoid feeding back forever. A reactor failure must produce a truthful failed outcome, not claim an ack succeeded when appending failed.

## Claimed paths

`.context/bin/`, `.context/EVENTLOG.md`, `.context/workspace.env`, `.context/handoffs/cursor-committer.md`, `.context/handoffs/doc-worker.md`, `.context/eventlog.toml` (only if needed), `.claude/settings.json`, `AGENTS.md`, `Drovefile`, `drove/`, `examples/log-driven/`, `docs/drovefile.md`, `docs/eventlog-reactors.md` (new operational runbook), `tests/`, `src/cli.rs`, `src/dsl.rs`, `Makefile`, `.github/workflows/ci.yml`, `.gitignore` (ignore .worktrees).

Do not edit `.context/DECISIONS.md`, the live log, other handoffs/reports, or unrelated product behavior. Tests in src may need example-string updates only.

## Verification and completion

Add focused integration tests with real eventlog in disposable git repos and stub model binaries: scoped commit and unrelated staging, commit ack driving docs, doc result committed once without feedback, action failure, restart/resume without duplicates, graceful shutdown/lock release. Never run tests on the production log. Ensure meaningful shell tests run locally and in CI (install eventlog explicitly if necessary, no silent skip that pretends coverage). Run appropriate formatting/tests and required Rust CI checks. Coordinate any external dependency/build blocker with the controller.

Open one concrete PR; monitor automated review and CI, fix or answer comments, resolve all threads. Report the PR URL early. Finish with changed paths, test evidence, exact cutover commands, and PR DONE once reviewed/green/current. If GitHub self-review restrictions apply, the reviewer may publish an explicit APPROVE verdict as a review comment; do not claim a formal approval that GitHub disallows.
