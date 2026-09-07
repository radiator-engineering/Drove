# Brief: pr14-was-rename (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v3-core-and-flavors-design.md`, decision **D34** (and the `drove lint` part of D26), test §10 D34. The nested `Action { Core, Herdr, Radiator }` and the ownership tokens you match on landed in PR #12; read the merged planner on `main` before writing anything.

Worktree: `~/Development/Drove-worktrees/pr14-was-rename`, branch `v3/pr14-was-rename`.

## Scope: PR 14, rename without loss (D34)

This is the Terraform `moved` block for Drove. Renaming a pane or workspace today abandons the live one (`Detach`) and creates a new one, because identity is the `drove_name` token.

1. **Model and DSL.** Add `was: Option<String>` to `Pane` and `Workspace` in `src/model.rs`, and the keyword `was = "old-name"` to `pane(...)`, `caller_pane(...)` if it exists on `main` yet (otherwise skip it), and `workspace(...)` in the prelude in `src/dsl.rs`. Validation: `was` must differ from `name` and must not equal any other declared name in the profile.
2. **Planner rule.** In `src/planner.rs`: when the backend snapshot holds a resource whose `drove_name` token equals a declared resource's `was` and holds no resource whose `drove_name` equals its `name`, emit `Core(RenamePane)` or `Core(RenameWorkspace)` (label to the new name) plus a token rewrite to `drove_name = name`, and **no** `Detach` and no create. If both old and new names exist live, emit `Conflict`. Ordering: the rename phase, same as label renames.
3. **State.** `src/state.rs`: the state file records the rename so the next run treats `was` as inert.
4. **`drove lint`.** New subcommand in `src/cli.rs` (D26, minimal): compile the Drovefile and warn for each `was` that matches nothing live, and for each task without `check`. Exit 0 with warnings printed; do not gold-plate beyond these two rules.
5. **Tests (spec §10 D34):** declared `was` with a matching live token → the plan contains `RenamePane` and no `Detach` and no `CreatePane`; both names live → `Conflict`; no match → `lint` prints the warning; workspace variant of the first case; state round-trip.

Claimed paths: `src/model.rs` (one field each on pane and workspace), `src/dsl.rs` (the `was` keyword only), `src/planner.rs`, `src/state.rs`, `src/cli.rs` (the `lint` subcommand only).

A second worker, `pr13-dsl-v3`, runs in parallel and reshapes the prelude in `src/dsl.rs` and `src/model.rs`. Keep your edits to those files small and additive. Whichever of you merges second rebases onto the other.

Out of scope: `docs/`, the IR and backends, any other DSL change.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
