# Brief: pr25-prune-state (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything. Do not append to the log.

Contract: section 7 (D48) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (read it from that path; it may not be on `main` yet), on top of D21–D24 (content-addressed resources, adoption) in the v2 spec and D43 in the v4 spec. The spec section is the whole requirement; read it first. GitHub issues radiator-engineering/Drove#24, #20 and #22 describe the live failures.

Worktree: `~/Development/Drove-worktrees/pr25-prune-state`, branch `v4/pr25-prune-state`.

## Scope: PR 25, prune local state against the live snapshot (D48)

1. `src/state.rs`: add `prune_missing(managed: &ManagedProfile, snapshot: &SessionSnapshot) -> (ManagedProfile, Vec<String>)`, pure. A resource is missing when its `backend_id` is not among the snapshot's workspace, tab or pane ids for its `kind` (`workspace`, `placement` = tab, `pane`). Dropping a workspace drops every placement and pane whose id starts with `<ws>:`; dropping a placement drops the panes under that tab. Return the dropped identities sorted.
2. `src/cli.rs`: at the one place local state becomes the planner's snapshot (`managed.to_snapshot(...)` in `up_command`, and the equivalent in the `plan`/`status` path), fetch the live snapshot first and prune. `up` saves the pruned managed set before applying; `plan` and `status` never write. `status` gains a `recreate` reason per pruned resource and the JSON reports gain `"pruned": [ids]`.
3. `src/planner.rs`: nothing should change; if the pruned snapshot is not enough for the planner to emit creates, stop and report why in the PR body instead of widening scope.
4. Tests per the spec's list: unit tests in `src/state.rs`, and end-to-end tests in `tests/cli.rs` using the existing fake Herdr harness (see how `up` tests build a fake session) with a state file that records a workspace the fake session lacks.
5. Reproduce first: from `main`, write a state file for a temp repo that records workspace `w9`, run `drove --json status` against a fake snapshot without it, and see `in_sync`. After your change it reports `recreate`.
6. `CHANGELOG.md`: one `### Fixed` bullet under `## [Unreleased]`.

Claimed paths: `src/state.rs`, `src/planner.rs`. Shared with other in-flight PRs (edit with targeted changes only, never a whole-file rewrite, and rebase on `origin/main` whenever GitHub says the branch is behind): `src/cli.rs`, `tests/cli.rs`, `CHANGELOG.md`. Another worker (`pr24-down-session`) is editing `down_command` in `src/cli.rs` and `src/backend/herdr.rs`; do not touch those.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
