# Brief: pr32-apply-progress (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs and reports, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log. Other workers are editing the same repo in parallel; the "Shared" paths below get targeted edits only, never a whole-file rewrite, and you rebase on `origin/main` whenever GitHub says the branch is behind.

Contract: section 11 (D52) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (read it from that path; the section is not on `main` yet), on top of D21 to D24 (v2 spec) and D43 (section 1 to 4 of the v4 file). The spec section is the whole requirement; read it first. Background: `.context/reports/audit-partial.md` findings 1 and 4 give file:line anchors and the repro.

Worktree: `~/Development/Drove-worktrees/pr32-apply-progress`, branch `v4/pr32-apply-progress`.

## Scope: PR 32, apply records progress as it happens (D52)

Implement the four numbered points of section 11 and its tests. Reproduce first: from `main`, a fake backend whose second create fails leaves state empty and a rerun plans all three creates again. Keep `record_ownership`'s logic; split it so each action's ownership is written right after that action succeeds. Dependency skipping uses the plan's own ordering and parent relations; do not invent a new graph.

Claimed paths: `src/executor.rs`. Shared: `src/cli.rs` (`up_command` printing and JSON for failures, skips and `interrupted`; `status` journal listing; `run_command` one line; worker pr31 is editing `up_command` before the plan is built and `lint_command`, stay below the plan), `src/state.rs` (journal read helpers only; pr31 owns `load`), `tests/cli.rs`, `docs/drovefile.md` (the `drove up` failure paragraph), `CHANGELOG.md`. Do not touch `src/planner.rs` or `src/backend/`.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
