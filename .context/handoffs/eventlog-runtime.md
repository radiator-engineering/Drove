> Archived 2026-09-07: this assignment is superseded. The worker workspace and worktree are closed. See `.context/reports/eventlog-handoff.md` for the archive and upstream scope. Do not resume this brief.

# Runtime implementation worker — Terra/high

You were spawned by the Drove controller. Use the requested gpt-5.6-terra/high model for this process/lifecycle task. Do not spawn subagents. Do not append to the log. Do not run doctor/init against the production repository; the controller already did setup. Never operate the live Herdr reactors. Work only in `/Users/jjmartin/Development/Drove/.worktrees/eventlog-runtime`, branch `fix/eventlog-runtime`, based on main.

You MAY commit only in this worker branch. Do not push or open a separate PR. The Luna worker will import your commit into `fix/eventlog-reactors` for a combined PR, independently reviewed by Sol (`eventlog-review`). Your deliverable is a tested commit SHA, exact changed paths, validation evidence and integration/cutover commands. Reply via your Herdr transcript, not collaboration tools. Your contract supersedes the older broad migration worker brief for the paths below.

## Scope and fixed interface

Implement production-quality one-pass reactor actions and thin launchers backed by the existing `eventlog` CLI. Read `/Users/jjmartin/Development/event-log/src/react/{mod,voter,action,lock}.rs`, `src/cmd/react.rs` and the reference docs. This sibling repo is read-only; do not modify/build/install into it. Installed CLI is eventlog 0.1.0.

Claimed paths: `.context/bin/` (including a new `test-reactors.py` Python stdlib unittest integration suite), `.context/workspace.env`, `Makefile`, `.github/workflows/ci.yml`. Do not edit anything else. Integration tests may create disposable git repos and call real eventlog with stub Cursor/Claude binaries; never test against production log. Keep Python version support reasonable for CI macOS/Linux. Avoid new dependencies when stdlib suffices.

Preserve these external commands for Luna wiring:

- `bash .context/bin/run-reactor.sh cursor-commit-reactor.sh`
- `bash .context/bin/run-reactor.sh doc-sync-reactor.sh`
- `bash .context/bin/reactor-lifecycle.sh start AGENT MODEL ROLE`
- `bash .context/bin/reactor-lifecycle.sh stop AGENT`

The start/stop helper uses eventlog append as controller, idempotently ensuring a live spawn and correct doc-worker claim `docs,README.md,AGENTS.md`. No blind duplicate spawn that drops claims. Stop records a no-path result if needed plus retirement only for an open lifecycle. It never stops processes itself. `run-reactor.sh` sources workspace.env and execs native `eventlog react` with proper --as/--on/--filter/--timeout flags. It must not own a bespoke tail/lock/supervisor loop. Native output includes watching for Drove readiness. `.context/bin/cursor-commit-reactor.sh` and `doc-sync-reactor.sh` can become one-pass actions. Make the names/usage explicit in comments.

## Required behavior

1. Preserve models Composer 2.5 Fast and Claude Sonnet, bounded timeouts, auth (unset ANTHROPIC_API_KEY unless DOC_USE_API_KEY=1), no AI attribution, current scoped-commit/doc-owner policy. A docs-only result must never recursively trigger another Claude pass: doc action looks up the committer ack's seq_done original event and rejects origin doc-worker, while retaining compatibility with old ack.origin if present.
2. Committer acts on result paths from EVENTLOG_PATHS; no paths means skip. Require actual scoped dirty work; never sweep unrelated pre-staged files. Refuse out-of-scope staged changes or use a robust isolated index strategy. Prompt contains actual scopes and driving seq/reference. Verify HEAD movement and return committed with actual commit ref(s), skipped for no change, failed/retryable for model failure. If model commits then exits nonzero, report the actual committed effect without retrying it. Verify actual committed paths and let runtime/explicit diagnostics report violations. Never manufacture success.
3. Doc action handles only committed cursor-committer acks, validates commit refs, detects exact actual changed authorized files including new/deleted docs, preserves existing unrelated dirty work, and appends a result --as doc-worker with exact paths only when it changed files. Never `git commit` in the doc action. Do not treat pre-existing dirty docs as newly generated. Record out-of-scope modifications as failures/violations instead of including them in doc results. Preserve useful diagnostic files on failures; clear success temp files.
4. Native action runner currently pipes stdout without draining until completion and kills only its direct action child on timeout. Redirect verbose model output; provide an inner bounded model runner that terminates the entire model process group before the outer native timeout. Stdlib Python subprocess is acceptable. Avoid replacing the native reactor runtime with a new custom loop. Native retryable retries once; failed advances its cursor, so document truthful failure and retain diagnostics.
5. Native react test executes the action even though runtime log appends are suppressed; disposable repos only. Never manually rm a reactor lock directory. Start/resume must not silently advance old production history; controller owns audited recovery.
6. Tests must prove exact commit paths/unrelated staging, real ack->docs->result->commit chain with one Claude invocation, no-change behavior, failure diagnostics and model child cleanup, resume without duplicate commits, clean shutdown/lock release, lifecycle idempotence and proper doc claims. Run them locally and wire a CI job installing a pinned real eventlog CLI. Determine package/repo names from sibling source/remote; do not guess. Existing Rust checks remain required for combined PR but this worker need only focused suite and shell syntax/Python compile.

Provide concrete, minimal implementation rather than merely listing recommendations. The old Luna draft is deliberately being discarded for these paths; start from this worktree's main baseline. If an interface adjustment is needed, report it before changing the frozen command shapes.
