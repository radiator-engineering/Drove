# Brief: pr26-root-tab (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log.

Contract: section 8 (D49) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (read it from that path; it may not be on `main` yet), on top of the Herdr flavor rules (D27–D37) in the v3 spec. The spec section is the whole requirement; read it first. GitHub issue radiator-engineering/Drove#25 describes the live failure.

Worktree: `~/Development/Drove-worktrees/pr26-root-tab`, branch `v4/pr26-root-tab`.

## Scope: PR 26, the first tab reuses Herdr's root tab (D49)

1. `src/backend/herdr.rs`: `HerdrClient::create_workspace` also captures the root tab id from the `workspace.create` response (`root_pane.tab_id`, or `tab.tab_id`). Expose it without changing the `Backend::create_workspace` signature if you can (for example a `created_root_tabs` map on the client, or return it through a Herdr-only method); if the trait must change, keep the Radiator implementation returning `None`. `HerdrExt::create_tab` gains `existing_tab: Option<&str>`: when set, apply the layout onto that tab id and `rename_tab` it to the declared label instead of opening a new tab.
2. `src/executor.rs`: for the first `CreateTab` in a workspace that this same apply created, pass the captured root tab id. Never for a workspace that was adopted or already existed.
3. `src/backend/mod.rs`: only if the trait signature must change (see 1).
4. Tests per the spec's list, in `src/backend/herdr.rs` (the existing fake-Herdr recorder tests such as `create_tab_opens_the_first_pane_splits_the_rest_then_sets_ratios_last` show the pattern) and `tests/herdr_contract.rs` (the `workspace.create` fixture gains the root tab). `tests/herdr_smoke_live.rs` is a live smoke test; extend it only if it already runs in CI.
5. Reproduce first: from `main`, run the fake-Herdr apply for a two-tab workspace and count the tabs created (three: root plus two). After your change: two.
6. `CHANGELOG.md`: one `### Fixed` bullet under `## [Unreleased]`.

Claimed paths: `src/executor.rs`, `tests/herdr_contract.rs`. Shared with other in-flight PRs (edit with targeted changes only, never a whole-file rewrite, and rebase on `origin/main` whenever GitHub says the branch is behind): `src/backend/herdr.rs` (worker `pr24-down-session` is adding `stop_session` beside `ensure_session`; stay out of that area), `src/backend/mod.rs`, `CHANGELOG.md`. Do not touch `src/cli.rs`, `src/state.rs` or `src/planner.rs`.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
