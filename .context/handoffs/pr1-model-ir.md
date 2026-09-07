# Brief: pr1-model-ir (spawned PR worker, Sonnet)

You were spawned by the Drove controller to deliver **PR 1** of the v2 design. This brief wins over the generic worker rules in AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never commit on `main`. Never touch `.context/events.jsonl`. Do not start subagents.

You are in worktree `~/Development/Drove-worktrees/pr1-model-ir` on branch `v2/pr1-model-ir`. Work only there.

## Read first
- `docs/superpowers/specs/2026-09-06-drove-v2-design.md` — the design. Sections 2 (D1–D20), 3, 4, 5 and the PR 1 row of section 7 are your contract.
- `.context/handoffs/recon-dsl-critique-report.md` and `recon-herdr-api-report.md` for background.
- Current code: `src/model.rs`, `src/dsl.rs`, `src/planner.rs`, `src/executor.rs`, `src/herdr.rs`, `src/cli.rs`.

## Scope (PR 1 only)
1. **Model v2** in `src/model.rs`: the five resource kinds (workspace, tab, pane, agent, task) with the fields in spec §3, `schema_version = 2`, validation rules from spec §4 (name regex, one `adopt` per profile, `ratios` length and range, `after` DAG, `extends` resolution, unique pane names per profile). Remove the v1 shapes (D20).
2. **IR** in `src/ir.rs`: a flat, canonical, serde JSON document `{schema_version: 2, profile, resources: [...]}` where each resource has `kind`, `name`, `parent` (address of the enclosing resource or null) and its fields. Deterministic ordering. `Profile::to_ir()` and `Ir::from_json()` round-trip.
3. **DSL prelude v2** in `src/dsl.rs`: `workspace`, `tab`, `pane`, `agent`, `task`, `profile`, `any_of`, `output`, `port`, `cmd` as in spec §4. `profile()` must return the profile value it registers. `extends` and `without` resolved at compile time. Keep the sandbox, `load()` rules and source digest exactly as they are.
4. **Backend trait** in `src/backend/mod.rs`: `trait Backend` with `capabilities() -> Capabilities` (the table in spec §3 as booleans plus `caller_pane_id()`), and the operation surface the planner will need: `snapshot`, `create_workspace`, `create_tab`, `split_pane`, `close_pane`, `set_ratio`, `rename_{workspace,tab,pane}`, `start_agent`, `prompt_agent`, `process_info`, `report_tokens`. Move `src/herdr.rs` behind `src/backend/herdr.rs` implementing the trait with the operations it already has; leave new operations as `unimplemented!()` with a `// PR 3` comment. PR 3 fills them.
5. **`drove render [--profile]`** in `src/cli.rs` printing the IR. Keep `status`, `plan`, `up` compiling against the new model even if their behaviour is reduced; PR 2 rewrites the planner. Do not implement hazards, hooks or `drove run`.
6. **Docs**: rewrite `docs/drovefile.md` for v2 and update `examples/basic/Drovefile`. Add `examples/log-driven/Drovefile` and `examples/log-driven/drove/reactors.star` verbatim from spec §4.
7. **Tests**: unit tests for every validation rule, an IR round-trip test, and a DSL compile test for `examples/log-driven/Drovefile`. `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` must pass.

## How to work
- Use TDD: write the failing test, then the code.
- Small commits with plain messages. No AI attribution trailers.
- When done: push the branch and open the PR with `gh pr create --base main --title "v2: model, IR, DSL prelude, backend trait" --body-file <file>`. The body lists what changed, what is deliberately left for PR 2 and 3, and how you verified it.
- Final reply to the controller: the single line `PR READY: <pr url>`. If blocked, reply `BLOCKED: <one line>` and stop.

## After the PR opens
Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
