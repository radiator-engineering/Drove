# Brief: pr33-planner-truth (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs and reports, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log. Other workers are editing the same repo in parallel; the "Shared" paths below get targeted edits only, never a whole-file rewrite, and you rebase on `origin/main` whenever GitHub says the branch is behind.

Contract: section 12 (D53 and D54) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (read it from that path; the section is not on `main` yet), on top of D21 to D24 (v2 spec) and D27 to D37 (v3 spec). The spec section is the whole requirement; read it first. Background: `.context/reports/audit-planner.md` gives the edit-case table with file:line anchors and the drift sketch.

Worktree: `~/Development/Drove-worktrees/pr33-planner-truth`, branch `v4/pr33-planner-truth`.

## Scope: PR 33, the planner never claims success for an edit it cannot apply, and detects command drift (D53, D54)

Implement D53 points 1 to 5 and D54 points 1 to 3 and their tests. Reproduce first: from `main`, change a pane's `cwd` in a Drovefile against a fake session and watch `drove plan` emit `RenamePane` and `up` record the new digest. For D53 point 1's exception, run `herdr api schema --json` (Herdr 0.8.2 is installed) and check whether the run-command verb takes a cwd; state the answer in the PR body either way. Drift needs `process_info` in the planner's snapshot: read how `ManagedProfile::to_snapshot` and the live snapshot are merged in `src/cli.rs` and pass the live `process_info` through with the smallest change; if that needs a new field on `SessionSnapshot` or `PaneInfo`, add it.

Claimed paths: `src/planner.rs`, `src/ir.rs`, `docs/drovefile.md` (a new "Editing a running layout" section and the `drove status` reasons list). Shared: `src/executor.rs` (only the `RestartCommand` cwd argument if D53 point 1's exception applies; worker pr32 is rewriting the apply loop, stay out of it), `src/backend/mod.rs` and `src/backend/herdr.rs` (only the `run_command` cwd parameter and any snapshot field you need; worker pr31 is editing `snapshot`), `src/cli.rs` (`status` `drift` reason and JSON `drifted` only), `tests/cli.rs`, `CHANGELOG.md`.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
