# Brief: pr11-backend-seam (spawned PR worker, Opus 4.8)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v3-core-and-flavors-design.md`, decisions **D27, D28, D29, D30** and tests §10 for each. The shapes in §3 and §4 are pinned: implement them as written. If one cannot work, stop and report `BLOCKED` with the reason; do not improvise a different shape, because PR 12 and PR 13 are built against these exact types.

Worktree: `~/Development/Drove-worktrees/pr11-backend-seam`, branch `v3/pr11-backend-seam`.

## Scope: PR 11, the core/flavor seam (D27–D30)

This is the architecture PR. Two smaller PRs run alongside you and merge first: PR 9 (a new `src/paths.rs` and a one-line call site in `src/planner.rs`) and PR 10 (method bodies only in `src/backend/radiator.rs`). Before you open your PR, `git fetch origin && git rebase origin/main` so you sit on top of both.

1. **D27 — split the trait.** In `src/backend/mod.rs`, reduce `Backend` to the intersection in spec §3, add `PaneSpec`, add the `herdr()` and `radiator()` accessors with default `None`, and shrink `Capabilities` to the six graded flags. Add `HerdrExt` (spec §3) and an empty `RadiatorExt`.
2. **D28 — implement it.** `src/backend/herdr.rs`: `HerdrClient` implements `Backend` and `HerdrExt` and overrides `herdr()` to `Some(self)`. `create_pane` with no placement creates the pane in the workspace's first tab. `src/backend/radiator.rs`: implement the reduced `Backend` only; move nothing into an extension; delete the tab/split no-ops. Keep PR 10's method bodies intact.
3. **D29 — nest the actions.** In `src/planner.rs`, replace `ActionKind` with `Action { Core, Herdr, Radiator }` and the three inner enums in spec §4. Keep every field each v2 variant carried, and keep `RANK_*`/`PHASE_*` ordering so the existing ordering tests pass unchanged. In `src/executor.rs`, one exhaustive match; a flavor action whose accessor returns `None` produces `Outcome::Unsupported { flavor, action }`.
4. **D29 — placement in the IR.** In `src/ir.rs`, add `Placement` (tagged enum, `flavor` discriminator) and `placement: Option<Placement>` on the pane resource; remove the `tab` resource kind; bump `IR_SCHEMA_VERSION` to 3. The Herdr flavor derives tabs from the placements of the panes that name them.
5. **Keep the v2 DSL working.** PR 13 changes the DSL later. In this PR, `src/dsl.rs` keeps accepting `tab(...)`/`tabs=[...]`; the compiler maps that tree onto `Placement::Herdr` per pane. Touch `src/dsl.rs` only in the compile-to-IR path. PR 12 is adding new prelude definitions at the top of `PRELUDE` at the same time; stay out of that region.
6. **D30 — two digests.** Pane content digest covers core fields only and excludes `placement`; each placement group gets a topology digest over `(tab, split, ratios, ordered pane names)`. Content change → `RestartCommand`; topology change → replace (D22). Update `src/state.rs` if the state file needs the topology digest.
7. **Tests, per spec §10:**
   - A test that reads `src/backend/mod.rs`, `src/planner.rs`, `src/ir.rs`, `src/executor.rs` and fails if the identifier `tab` appears in any of them.
   - A fake backend whose `herdr()` is `None` receives `Herdr(CreateTab)`; executor returns `Unsupported`; core panes in the same plan are still created.
   - All v2 planner tests pass with the nested `Action`.
   - Compile `examples/basic` and `examples/log-driven` and assert every pane's content digest equals the v2 value (record the v2 values in the test before you change the digest code). Move one pane between tabs; content digest unchanged, topology digest changed.
   - `scripts/smoke-herdr.sh` stays green.

Claimed paths: `src/backend/mod.rs`, `src/backend/herdr.rs`, `src/backend/radiator.rs` (trait restructure only), `src/planner.rs`, `src/ir.rs`, `src/executor.rs`, `src/state.rs`, `src/dsl.rs` (compile-to-IR path only), `src/readiness.rs` if compiling requires.

Out of scope: `src/cli.rs` beyond what compiling requires (PR 12 owns it), `src/backend/select.rs` (PR 12 creates it), any DSL surface change (PR 13), `was=` (PR 14).

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
