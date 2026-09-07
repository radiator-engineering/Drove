# Brief: pr13-dsl-v3 (spawned PR worker, Opus 4.8)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v3-core-and-flavors-design.md`, decision **D31** and section **9** (compatibility), tests §10 D31 and D30. The IR types you compile onto (`Placement`, the pane resource) landed in PR #12 and are pinned in spec §4; the prelude namespace objects `herdr` and `radiator` landed in PR #11 with `herdr.session()` / `radiator.hub()`. Read the merged code on `main` before writing anything. Do not change the IR or backend types; if you must, stop and report `BLOCKED`.

Worktree: `~/Development/Drove-worktrees/pr13-dsl-v3`, branch `v3/pr13-dsl-v3`.

## Scope: PR 13, the v3 DSL surface (D31, §9)

1. **Groups.** `herdr.tab(name, panes, split = herdr.RIGHT, ratios = [])` returns a group value. Add the constants `herdr.RIGHT` and `herdr.DOWN`. The strings `"right"`/`"down"` are still accepted for one release and produce a warning naming the constant.
2. **Core `workspace`.** `workspace(name, panes = [...])` is the core signature. `panes` accepts `pane` values and groups. The compiler flattens groups into the core pane list and writes each pane's `placement = Placement::Herdr { tab, split, ratios }`. A pane given directly (not in a group) has `placement = None`.
3. **Shims, one release each, every one with a warning that prints the v3 rewrite:** `workspace(tabs = [...])` → `panes = [herdr.tab(...)]`; bare `tab(...)` → `herdr.tab(...)`; `adopt = "caller"` → `caller_pane(...)`.
4. **`caller_pane(name, ...)`** replaces `adopt = "caller"`; same fields as `pane` minus `adopt`; the at-most-one-per-profile rule is unchanged.
5. **Values over names.** `profile()` returns the value it registers (already true; keep it). `extends`, `without` and `after` accept values or name strings; the documented form is the value (`extends = default`, `without = [files]`). Name strings stay valid with no warning.
6. **Warnings channel.** Compilation returns warnings alongside the config. `drove render` and `drove plan` print them. `drove render` on a v2 Drovefile also prints a "v3 form" block: the file rewritten with the shims applied.
7. **Migrate the examples** `examples/basic/Drovefile` and `examples/log-driven/Drovefile` to the v3 form (no shims). `scripts/smoke-herdr.sh` stays green.
8. **Tests (spec §10 D31 and D30):** prelude tests for `herdr.tab` flattening (placement written per pane, order preserved), `caller_pane`, value `extends`/`without`/`after`, and each shim's warning text; the D30 digest-stability test: compile each example in its v2 form (keep copies under `tests/fixtures/v2/`) and in its migrated v3 form and assert every pane's content digest is identical.

Claimed paths: `src/dsl.rs`, `src/model.rs`, `src/cli.rs` (render/plan warning output only), `examples/`, `tests/`.

A second worker, `pr14-was-rename`, runs in parallel and adds only a `was` field on pane and workspace, a `was =` keyword in the prelude, a planner rule, and a `drove lint` subcommand — small, additive edits in `src/model.rs`, `src/dsl.rs`, `src/cli.rs`. Whichever of you merges second rebases onto the other; keep your edits to those files additive where you can so the rebase is clean.

Out of scope: `docs/` (PR 15), the IR and backends (PR 12), `was =` (PR 14).

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
