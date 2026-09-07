# Brief: pr31-live-plan (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs and reports, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log. Other workers are editing the same repo in parallel; the "Shared" paths below get targeted edits only, never a whole-file rewrite, and you rebase on `origin/main` whenever GitHub says the branch is behind.

Contract: section 10 (D51) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (read it from that path; the section is not on `main` yet), on top of D46, D47, D48 and D50 in the same file. The spec section is the whole requirement; read it first. Background: `.context/reports/audit-herdr.md` findings 1 to 4 and `audit-state.md` findings 3, 5, 6 and `audit-partial.md` finding 3 give file:line anchors and repro steps.

Worktree: `~/Development/Drove-worktrees/pr31-live-plan`, branch `v4/pr31-live-plan`.

## Scope: PR 31, plan from the live session (D51)

Implement the eight numbered points of section 10 and its tests. Start with point 8 (the error-scriptable fake Herdr) because points 1 and 2 test through it. Reproduce point 1 first: from `main`, state that records ids for a session that is not running, run `drove up` with the fake starting the session on `ensure_session`, and see either `AlreadyRunning` or a focus failure.

Claimed paths: `src/backend/select.rs`, `src/state.rs` (`LocalState::load` only), `docs/drovefile.md` (target resolution and `default` paragraphs). Shared: `src/cli.rs` (`up_command` after `ensure_session`, `lint_command`, status JSON fields only; worker pr32 owns the apply loop in `src/executor.rs` and its `up` reporting), `src/backend/herdr.rs` (`snapshot`, `caller_pane_id`, `stop_failed_because_not_running`, the fake in its tests), `tests/cli.rs`, `CHANGELOG.md` (one `### Fixed` bullet per point under `## [Unreleased]`). Do not touch `src/planner.rs`.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
