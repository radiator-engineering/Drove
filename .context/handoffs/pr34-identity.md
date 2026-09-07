# Brief: pr34-identity (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs and reports, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log. Other workers are editing the same repo in parallel; the "Shared" paths below get targeted edits only, never a whole-file rewrite, and you rebase on `origin/main` whenever GitHub says the branch is behind.

Contract: section 13 (D55) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (read it from that path; the section is not on `main` yet), on top of D48 (section 7), D51 (section 10) and D16 (v2 spec). The spec section is the whole requirement; read it first. Background: `.context/reports/audit-state.md` findings 2 and 4 and `audit-herdr.md` "Harness gaps" 2 and 2b.

Worktree: `~/Development/Drove-worktrees/pr34-identity`, branch `v4/pr34-identity`.

## Scope: PR 34, identity beyond the id string (D55)

Implement the three numbered points of section 13 and its tests. Reproduce first: from `main`, a state file recording workspace `w1` labelled `control` against a fake snapshot whose `w1` is labelled `other` plans a `RenameWorkspace` of the stranger instead of a create. Point 2's rule for telling a reused id from a user rename must be written down in `docs/drovefile.md` in one paragraph.

Claimed paths: `src/state.rs`, `tests/herdr_contract.rs`, `docs/drovefile.md` (state section). Shared: `src/cli.rs` (the `starting fresh` line and the prune reason wording), `src/executor.rs` (recording `label`/`cwd` at apply time only), `tests/cli.rs`, `CHANGELOG.md`. Do not touch `src/planner.rs`.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
