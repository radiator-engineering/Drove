# Brief: pr5-radiator-backend (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `docs/superpowers/specs/2026-09-06-drove-v2-design.md` (D1-D26) and the code on `main` after PR 1 (model v2, `src/ir.rs`, `src/backend/mod.rs`). Read those before writing anything; do not change their public shapes, and if you must, stop and report BLOCKED.

Worktree: `~/Development/Drove-worktrees/pr5-radiator-backend`, branch `v2/pr5-radiator-backend`.

## Scope: PR 5, Radiator backend (spec section 3, D3, D23)
1. Add `src/backend/radiator.rs` implementing the Backend trait against the Radiator hub in `~/Development/radiator-cli` (Hub, Workspace `w{n}`, Pane `w{ws}:p{n}`, kinds term and chat; `pane.open`, `pane.run`, `runner.set_state`, `events.subscribe`). Read `.context/handoffs/recon-radiator-report.md` and `recon-radiator-gaps-report.md` first; the second lists what the hub lacks and the proposed additions.
2. `capabilities()` returns the Radiator column: no tabs, no splits, no ratios, `readiness_output = false` unless the gaps report shows output reading exists. Tabs in the IR map to nothing; every pane in a workspace becomes one hub pane.
3. Where the hub cannot store tokens, keep ownership in the local state journal keyed by hub pane id, and mark a pane `unknown` when the journal and the hub disagree (spec section 9, load-bearing risk).
4. Select the backend with `--backend radiator` or the `RADIATOR_HUB_SOCKET` environment variable when `HERDR_ENV` is unset.
5. Unit tests with a fake hub socket; a `scripts/smoke-radiator.sh` that runs against a hub started in a temp dir if the hub binary builds locally, otherwise document why it is skipped.

Out of scope: changes inside `~/Development/radiator-cli`; write proposed hub changes to `docs/radiator-backend.md` instead.

## Addendum (after recon-radiator-gaps)
Code against the protocol sketch at the end of `recon-radiator-gaps-report.md`: `pane.set_metadata`, `PaneInfo.metadata`, `PaneInfo.process`, `pane.tail`, `workspace.rename`, `hub.capabilities`. A parallel hub PR adds them. Detect their absence at runtime (method-not-found) and degrade: journal-only ownership, `readiness_output = false`, workspace rename skipped with a warning.

## After the PR opens
Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
