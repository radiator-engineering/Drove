# Brief: pr29-down-prune (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log.

Contract: section 9 (D50) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (read it from that path; it is not on `main`), on top of D47 (section 6) and D48 (section 7). The spec section is the whole requirement; read it first. GitHub issue radiator-engineering/Drove#29 describes the live failure.

Worktree: `~/Development/Drove-worktrees/pr29-down-prune`, branch `v4/pr29-down-prune`.

## Scope: PR 29, `down` never aborts on a resource the session already lost (D50)

1. `src/cli.rs` `down_command`: fetch the live snapshot the way `up_command` does after D48, run `state::prune_missing`, detach the pruned ids from state (no backend call), then call `down` on the rest. Unreachable session: warn and run `down` with no backend. Print and JSON per the spec.
2. `src/executor.rs` `down`: collect `close_pane` errors into `DownReport::close_failed` instead of returning them; keep detaching and saving.
3. `tests/cli.rs`: the three end-to-end tests the spec lists, using the existing fake Herdr harness and the `HERDR_BIN_PATH` fake from the D47 tests.
4. Reproduce first: from `main`, a state file recording pane `w1:p8` that the fake session lacks makes `drove down --purge` exit 1 with `pane_not_found`. After your change it exits 0 and the state file is empty.
5. Docs you own: the `drove down` paragraph in `docs/drovefile.md` (one sentence on pruning and warnings), and one `### Fixed` bullet under `## [Unreleased]` in `CHANGELOG.md`.

Claimed paths: `src/cli.rs` (`down_command` and its tests only), `src/executor.rs` (`down`, `DownReport` and their tests only), `tests/cli.rs`, `docs/drovefile.md`, `CHANGELOG.md`. Nothing else. If the change needs something outside these paths, stop and say so in your report instead of editing it.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
