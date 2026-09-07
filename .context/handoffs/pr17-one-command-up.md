# Brief: pr17-one-command-up (spawned PR worker, Opus 4.8)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (D41–D45; section 3 names your tests), on top of the v3 spec `2026-09-06-drove-v3-core-and-flavors-design.md` (D27–D40 in `.context/DECISIONS.md`).

Worktree: `~/Development/Drove-worktrees/pr17-one-command-up`, branch `v4/pr17-one-command-up`.

## Scope: PR 17, `up` gets you there (D43, D44)

Today `Command::Up` in `src/cli.rs` prints the plan and runs only its tasks; `executor::apply_plan` exists and is never called from the CLI (see the comment in that match arm). This PR makes `drove` the one command.

1. **Apply the plan.** After tasks, call `apply_plan` for the workspace/pane/agent actions with the same `--yes` gate for destructive actions that tasks use. `Conflict` still exits 2 without applying. Keep `plan`/`status` read-only.
2. **HerdrExt verbs (D44).** In `src/backend/mod.rs` add `focus_workspace(&self, id: &str) -> Result<()>` and `ensure_session(&self, name: &str) -> Result<SessionState>` with `enum SessionState { Running, Started, CannotStart { hint: String } }`. Implement both in `src/backend/herdr.rs`: focus sends `workspace.focus`; `ensure_session` returns `Running` when the socket answers `ping`, otherwise tries to start the session's server headlessly by shelling out to the `herdr` binary (`HERDR_BIN_PATH`, else `PATH`). **Verify against the installed Herdr what starts a named session's server without a TUI** (candidates: `herdr server --session NAME`, or `herdr --session NAME` with no TTY). Test with a throwaway session name and stop and delete it afterwards (`herdr session stop`, `herdr session delete`); never touch the `drove` or `default` sessions. If nothing starts it headlessly, `ensure_session` returns `CannotStart` with the hint `herdr --session NAME`, and the PR body says so; that is an acceptable outcome. Wait for the socket up to 10 s after a start. Radiator: accessor default only, no ext.
3. **Focus and attach (D43 step 4).** After a successful apply, focus the profile's first declared workspace or `--workspace NAME`. If `HERDR_ENV` is unset and stdout is a TTY, `exec` `herdr session attach NAME` (replace the process; on Windows spawn and wait). `--no-focus` skips both; `--json` implies `--no-focus`.
4. **Summary line** exactly as D43 step 5.
5. **Tests** per spec section 3, D43 and D44, against the existing fake Herdr in `src/backend/herdr.rs` tests and the recording backend in `src/executor.rs` tests. The attach exec is not unit-tested; gate it behind a function you can stub.

Claimed paths: `src/backend/mod.rs`, `src/backend/herdr.rs`, `src/backend/radiator.rs` (accessor default only), `src/executor.rs`, `src/cli.rs` (the `up` path and its flags only).

A second worker, `pr16-profile-targets`, edits argument parsing, profile resolution and `ls` in `src/cli.rs` in parallel. Do not touch the argument structs beyond adding `--workspace` and `--no-focus` to `Up`. Whichever merges second rebases onto the other; `--profile` still works for you either way.

Out of scope: D41/D42 (PR 16); docs and the repo Drovefile (PR 18).

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
