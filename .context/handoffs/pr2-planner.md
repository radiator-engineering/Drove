# Brief: pr2-planner (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `docs/superpowers/specs/2026-09-06-drove-v2-design.md` (D1-D26) and the code on `main` after PR 1 (model v2, `src/ir.rs`, `src/backend/mod.rs`). Read those before writing anything; do not change their public shapes, and if you must, stop and report BLOCKED.

Worktree: `~/Development/Drove-worktrees/pr2-planner`, branch `v2/pr2-planner`.

## Scope: PR 2, planner v2 (spec section 5 and D5, D9, D16, D17, D21, D22)
1. Rewrite `src/planner.rs` to plan over the IR against a `Snapshot` from the Backend trait. Actions: CreateWorkspace, RenameWorkspace, CreateTab, RenameTab, SplitPane, ClosePane, SetRatio, RenamePane, RestartCommand, AdoptPane, StartAgent, PromptAgent, RunTask, Detach, Conflict. Each carries the target IR name, the backend id when known, `destructive: bool`, and a one-line reason.
2. Ownership by tokens `drove_name`, `drove_profile`, `drove_digest` (D21). Same name and digest: converged. Same name, different digest: RestartCommand for a serve pane, or a destructive replace only for topology changes (D22). No token: unmanaged, never touched. Declared but absent: create. Owned but undeclared: Detach.
3. Forward-model hazards inside the planner (spec section 5): treat each task's declared `inputs` and outputs as reads and writes; order by `after`; flag ReadWrite and WriteWrite hazards as Conflict actions; independent actions may be marked parallel-safe.
4. Fresh tabs are planned as one layout application; existing tabs get incremental split, close and set-ratio actions (D9).
5. The `adopt="caller"` pane: if the backend reports a caller pane, plan AdoptPane (rename and token only); otherwise plan a normal create and set `adopted=false` in the state journal (D24).
6. Deterministic action ordering (workspace, tab, pane, agent, task; creates before renames before starts) and a stable text rendering used by `drove plan`.
7. Tests: table-driven planner tests with hand-built snapshots covering every rule above, including "unmanaged panes produce no actions" and "moved checkout does not change any digest".

Out of scope: executing actions, hooks, `drove run`, backend code.

## After the PR opens
Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
