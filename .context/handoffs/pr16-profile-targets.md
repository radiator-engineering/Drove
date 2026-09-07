# Brief: pr16-profile-targets (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (D41–D45; section 3 names your tests), on top of the v3 spec `2026-09-06-drove-v3-core-and-flavors-design.md` (D27–D40 in `.context/DECISIONS.md`).

Worktree: `~/Development/Drove-worktrees/pr16-profile-targets`, branch `v4/pr16-profile-targets`.

## Scope: PR 16, profile-scoped targets and the positional profile (D41, D42)

1. **Model.** `Profile { session: Option<String>, backend: Option<String> }` in `src/model.rs`. Validation: `backend` must be a known id (`herdr`, `radiator`). `extends` copies the parent's values unless the child sets its own.
2. **DSL.** `profile(name, workspaces=[], tasks=[], extends=None, without=[], session=None, backend=None)` in the prelude (`src/dsl.rs`). Nothing else in the DSL changes.
3. **Selection.** `select::resolve` in `src/backend/select.rs` gains a profile layer between env and file: flag > env > profile > file > built-in, for the backend id and for the target name. Keep the function pure; extend the precedence-table test.
4. **CLI (`src/cli.rs`, parsing and profile resolution only).** `drove [PROFILE]` and every subcommand take an optional positional `PROFILE`; `--profile` stays as an alias with no warning; both given and different is an error. No profile: `default` if declared, else the only profile, else exit 2 printing the profile list. Unknown: exit 2 with the list. Add `drove ls` (`--json` supported): one row per profile with backend, target name, and `reachable` (a `ping` on the resolved target; unreachable is a row value, not an error).
5. **Tests** per spec section 3, D41 and D42. Use `examples/log-driven/Drovefile` for `ls`; add a `monitoring` profile with `session = "drove-mon"` to that example to exercise it.

Claimed paths: `src/model.rs`, `src/dsl.rs`, `src/backend/select.rs`, `src/cli.rs` (argument parsing, profile resolution, `ls`), `examples/log-driven/Drovefile`, `tests/`.

A second worker, `pr17-one-command-up`, edits the `up` path in `src/cli.rs` and the Herdr backend in parallel. Keep your `cli.rs` edits to the argument structs, profile resolution and `ls`; do not restructure `run_with`. Whichever merges second rebases onto the other.

Out of scope: the `up` flow, focus, session start (PR 17); docs and the repo Drovefile (PR 18).

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
