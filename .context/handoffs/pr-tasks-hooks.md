# Brief: pr-tasks-hooks (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`. Then follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: you own every inline thread (CodeRabbit and cubic included) until resolved, keep the branch current, and stop only with `PR DONE: <url>` (or `BLOCKED: <one line>`). Never run drove up or any layout test against the live Herdr session; use an isolated named session and tear it down. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `docs/superpowers/specs/2026-09-06-drove-v2-design.md` (D1-D26) and the code on `main`, which now has model v2, `src/ir.rs`, `src/backend/mod.rs`, and planner v2 in `src/planner.rs`. Read those before writing; do not change their public shapes, and if you must, stop and report BLOCKED.

Worktree: `~/Development/Drove-worktrees/pr-tasks-hooks`, branch `v2/pr-tasks-hooks`.

## Scope: spec PR 4 — tasks, hooks, `drove run`, `drove down` (spec sections 3-5, D11, D12, D13, D14)
1. Executor for the planner's task actions in `src/executor.rs`: run a `task(run=, check=, inputs=, after=, auto=)` by running its `check` first and skipping when it passes (early cutoff, spec section 5); otherwise run `run`, honoring `after` ordering and the hazards the planner flagged. Approval-gate host argv on the digest of the task bytes, reusing the existing approval journal in `src/state.rs` (D16). Record each run in the state journal with its digest and outcome.
2. Hooks: `on_start` and `on_stop` per resource (D13). `on_start` runs after the resource converges; `on_stop` runs before a Detach or a `drove down`. Same approval gate as tasks.
3. `drove run <task> [--profile]`: run a single named task and its `after` prerequisites, nothing else. `drove run` with no task lists the tasks with their last outcome.
4. `drove down [--profile]`: run `on_stop` hooks, then Detach every owned resource in reverse dependency order. Never close unmanaged panes (D18). `--purge` additionally closes owned panes; without it, detach only.
5. `auto=true` tasks run during `drove up`; `auto=false` tasks run only via `drove run`.
6. Wire these into `src/cli.rs` and keep `status`, `plan`, `up`, `render` working.
7. Tests: executor tests with a fake backend and a temp state dir covering early cutoff (check passes -> skip), `after` ordering, approval gate (unapproved argv -> blocked, approved -> runs), hook firing on start and stop, `drove run` running only the target and its prerequisites, and `drove down` detaching in reverse order without touching unmanaged resources.

Out of scope: backend socket code (that is PRs 3 and 5), the log-driven example (PR 6), `drove watch` (PR 7).
