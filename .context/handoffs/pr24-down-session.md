# Brief: pr24-down-session (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log.

Contract: section 6 (D47) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md`, on top of D43, D44 and D46 in the same file. The spec section is the whole requirement; read it first.

Worktree: `~/Development/Drove-worktrees/pr24-down-session`, branch `v4/pr24-down-session`.

## Scope: PR 24, `drove down` stops and deletes a named Herdr session (D47)

1. `src/backend/mod.rs`: add `SessionStop { stopped: bool, deleted: bool }` and `HerdrExt::stop_session(&self, name: &str) -> Result<SessionStop>`.
2. `src/backend/herdr.rs`: implement it beside `ensure_session`, shelling out through `herdr_bin_path()` to `herdr session stop NAME` then `herdr session delete NAME`. Stop failing because the session is not running is not an error (Herdr answers `session_stop_failed` on stderr / JSON; use `--json` and read the result); delete still runs. A missing binary or a delete failure is an error. Unit-test with a fake `herdr` shell script on `HERDR_BIN_PATH` that records its argv to a file, the way the existing `ensure_session` tests stub the binary if they do; otherwise write that fixture.
3. `src/cli.rs` `down_command`: after `down(...)` returns, when the backend is Herdr and the resolved target has a name that is not `default` (`target.name`, see how `up_command` derives the session name for `ensure_session`), call `stop_session`. Print `stopped session NAME` (or `deleted session NAME` if it was already stopped); add `"session": {"name", "stopped", "deleted"}` to the JSON report, absent when no named session. Errors from `stop_session` propagate after the detach report has been saved.
4. `src/executor.rs`: no change expected; keep `down` pure of session logic.
5. `tests/cli.rs`: end-to-end tests per the spec's test list, driving `drove down` with `HERDR_BIN_PATH` pointed at the fake script and a Drovefile that declares `herdr.session("x")`; one test with no session declared shows the old output unchanged and no `herdr` invocation.
6. Docs you own for this PR: the `drove down` paragraph and command line in `docs/drovefile.md`, the `drove down` line in `README.md`, and an `### Added` bullet under `## [Unreleased]` in `CHANGELOG.md`.
7. Reproduce first: on `main`, `drove down` in a repo whose Drovefile names a session leaves `herdr session list` showing it `running`. After your change the session is gone from the list.

Claimed paths: `src/backend/mod.rs`, `src/backend/herdr.rs`, `src/cli.rs` (`down_command` and its tests only), `tests/cli.rs`, `docs/drovefile.md`, `README.md`, `CHANGELOG.md`. Nothing else. If the change needs something outside these paths, stop and say so in your report instead of editing it.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
