# Brief: pr3-herdr-backend (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `docs/superpowers/specs/2026-09-06-drove-v2-design.md` (D1-D26) and the code on `main` after PR 1 (model v2, `src/ir.rs`, `src/backend/mod.rs`). Read those before writing anything; do not change their public shapes, and if you must, stop and report BLOCKED.

Worktree: `~/Development/Drove-worktrees/pr3-herdr-backend`, branch `v2/pr3-herdr-backend`.

## Scope: PR 3, Herdr backend v2 (spec section 3 capability matrix, D9, D16, D21)
1. Fill every `unimplemented!()` in `src/backend/herdr.rs` using Herdr 0.8.2 protocol 20 over the NDJSON socket: `pane.split`, pane close, `layout.set_split_ratio`, pane rename, `agent.start`, `agent.prompt`, `pane.process_info`, and token storage through `report_metadata` (tokens `drove_name`, `drove_profile`, `drove_digest`). Read `.context/handoffs/recon-herdr-api-report.md` for the verified message shapes and `herdr --help` for the rest.
2. `snapshot()` must return tokens per pane, the caller pane id from `HERDR_PANE_ID` when set, and process info for each pane.
3. `capabilities()` returns the Herdr column of the matrix, with `readiness_output = true`.
4. Readiness probes: implement `output()` by reading recent pane output, and `port()` / `cmd()` host-side in `src/readiness.rs` (D23), each with a timeout.
5. Keep `scripts/smoke-herdr.sh` green against an isolated named session, and extend it to cover split, set-ratio, token round-trip and adopt.
6. Unit tests with a fake socket for request encoding; the smoke script for the live path.

Out of scope: planner, executor changes beyond what compiling requires, Radiator.

## After the PR opens
Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
