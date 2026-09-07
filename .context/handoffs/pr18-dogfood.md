# Brief: pr18-dogfood (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs, and nothing under `docs/superpowers/`. Do not start subagents. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`; the body lists what changed and how you verified it. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass. Read the merged code on `main` before writing anything.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v4-one-command-design.md` (D41–D45), on top of the v3 spec `2026-09-06-drove-v3-core-and-flavors-design.md`.

Worktree: `~/Development/Drove-worktrees/pr18-dogfood`, branch `v4/pr18-dogfood`. PR 16 (profile targets, positional profile, `ls`) and PR 17 (one-command `up`) are merged on `main`; read their diffs (`git log -3 -p main`) first.

## Scope: PR 18, Drove dogfoods itself, and the docs say so (D45)

1. **Repo-root `Drovefile`.** Model it on `examples/log-driven/Drovefile` and on what this repo actually runs: read `.context/workspace.env` and the layout script it names (`layout.sh`, wherever it lives) to see the real control / maintenance / files workspaces, their tabs, panes and commands, and declare exactly that as profile `default` in session `drove`. Add a `monitoring` profile in session `drove-mon` with one workspace containing two panes: `eventlog-view.sh --follow` (check the flag exists; else `--last 40` in a watch loop) and `git log --oneline -20`. The layout script itself does not change.
2. **Verify.** `cargo run -- render` on the Drovefile: no warnings. `cargo run -- plan` and `cargo run -- plan monitoring` against the live sessions: paste both outputs into the PR body. Do NOT run `up`, `down` or `run` against the `drove` session; the controller lives in it.
3. **Docs.** `README.md` quick start becomes: write a Drovefile, run `drove`; then a short "other commands" list (`drove ls`, `drove plan`, `drove status`, `drove render`, `drove lint`, `drove run`, `drove down`). `docs/drovefile.md`: document `profile(session=, backend=)`, the positional profile, the no-profile rules, and `drove ls`. Whichever doc describes `drove up` today gets the D43 behaviour (session start, apply, focus, attach, `--no-focus`, `--workspace`, summary line). Every Drovefile snippet you add or change must pass `cargo run -- render` with no shim warnings. Load the `plain-technical-english` skill and run its final gate on the prose.

Claimed paths: `Drovefile`, `README.md`, `docs/` (not `docs/superpowers/`), `examples/`.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.
