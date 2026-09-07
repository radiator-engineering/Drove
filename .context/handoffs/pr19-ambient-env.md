# Brief: pr19-ambient-env (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything.

Contract: section 5 (D46) of `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md`, on top of D41/D42 in the same file.

Worktree: `~/Development/Drove-worktrees/pr19-ambient-env`, branch `v4/pr19-ambient-env`.

## Scope: PR 19, ambient host env ranks below the profile (D46)

1. In `src/backend/select.rs`, split `EnvInputs` into explicit (`DROVE_BACKEND`, new `DROVE_SESSION`, new `DROVE_TARGET`) and ambient (`HERDR_SESSION`, `RADIATOR_HUB`, ambient Radiator detection) layers, and reorder `resolve` to: flag > explicit env > profile > file > ambient > built-in. Keep it pure. Update the doc comment that states the precedence.
2. In `src/cli.rs`, gather the two new variables where `EnvInputs` is built (around the `DROVE_BACKEND` read). Also replace the doc comment on the `--profile` flag: the clap arg-id note ("Named `profile_flag`…") shows up in `drove --help`; move that sentence into a normal `//` comment and leave only the user-facing sentence in the `///` doc.
3. Tests per D46. Also extend `tests/cli.rs`: `drove ls` on `examples/log-driven/Drovefile` with `HERDR_SESSION=drove` in the environment shows `monitoring` targeting `drove-mon`.
4. Reproduce first: from `main`, `HERDR_SESSION=drove cargo run -q -- --file examples/log-driven/Drovefile ls` prints `monitoring: … target=drove`. After your change it prints `drove-mon`.

Claimed paths: `src/backend/select.rs`, `src/cli.rs` (EnvInputs gathering and the `--profile` doc comment only), `tests/cli.rs`. Another worker, `pr18-dogfood`, owns `Drovefile`, `README.md`, `docs/` and `examples/`; if your change needs a docs line, say so in the PR body instead of editing docs.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
