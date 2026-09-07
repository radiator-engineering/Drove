# Brief: pr12-backend-select (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v3-core-and-flavors-design.md`, decisions **D32, D33** and tests §10 D32/D33. Read it before writing anything.

Worktree: `~/Development/Drove-worktrees/pr12-backend-select`, branch `v3/pr12-backend-select`.

## Scope: PR 12, the project declares its backend and target; the CLI honors it (D32, D33)

Today `src/cli.rs:12` constructs `HerdrClient` and nothing else. After this PR, `drove up|status|plan|down|run` reach the Radiator backend when selected.

1. **Prelude (D32).** In `src/dsl.rs`, add to the top of `PRELUDE`: a core `backend(id)` call, and two namespace objects `herdr` and `radiator` whose only members for now are `herdr.session(name)` and `radiator.hub(name)`. Each records its value on the compiled config. PR 13 adds `herdr.tab` and the rest to these same objects later; give them a shape a later PR can extend (a Starlark struct or module value, not a bare function). PR 11 edits the compile-to-IR path of `src/dsl.rs` at the same time; keep your changes to the prelude text and a small resolve step.
2. **Model.** In `src/model.rs`, `DroveConfig` gains `backend: Option<String>` and per-flavor `target` declarations (`herdr_session: Option<String>`, `radiator_hub: Option<String>`, or one small struct). `Option` + `#[serde(default)]` so v2 Drovefiles stay valid under `deny_unknown_fields`.
3. **Resolution (D32).** Implement the four-level order in spec §5: CLI (`--backend`, `--target`; `--session` stays as a Herdr alias of `--target`; `--socket` stays as an explicit override) > env (`DROVE_BACKEND`; `HERDR_SESSION` / `RADIATOR_HUB` per backend; `HERDR_SOCKET_PATH` as today) > Drovefile > built-in (`herdr`; Herdr `default`; Radiator `main`). Put it in a pure function so the precedence table test needs no I/O. `resolve_socket_path` in `src/backend/herdr.rs` already encodes the Herdr env rules; call it, do not duplicate it.
4. **Factory (D33).** New file `src/backend/select.rs`: `pub fn open(id: &str, target: &Target) -> Result<Box<dyn Backend>>`. Read `RadiatorClient`'s constructor in `src/backend/radiator.rs` for how it locates a hub socket by name. Register the module with a single `pub mod select;` line at the top of `src/backend/mod.rs` — that one line is your only touch on `mod.rs`; PR 11 is rewriting the rest of it concurrently.
5. **CLI.** Every subcommand obtains its backend through `select::open` and passes `&dyn Backend` (`Box<dyn Backend>` held by `main`). Remove the direct `HerdrClient` construction. Unknown backend id → a clear error listing the known ids.
6. **Tests, per spec §10.** A precedence table test over flag × env × file × built-in for both backends. `select::open("radiator", ..)` returns a backend whose `herdr()` accessor is `None` — until PR 11 lands that accessor does not exist, so assert on `capabilities()` instead and leave a `TODO(pr11)` comment naming the accessor assertion. A compile test that `backend("radiator")` and `radiator.hub("main")` round-trip into `DroveConfig`.

Claimed paths: `src/cli.rs`, `src/backend/select.rs`, `src/model.rs` (new fields only), `src/dsl.rs` (prelude text and resolve step only), `src/backend/mod.rs` (the one `pub mod select;` line).

Out of scope: the `Backend` trait, `Capabilities`, the planner, the IR, any other DSL constructor.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
