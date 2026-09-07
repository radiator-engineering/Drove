# Brief: pr9-windows (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v3-core-and-flavors-design.md`, decision **D36** and test §10 D36. Read it before writing anything.

Worktree: `~/Development/Drove-worktrees/pr9-windows`, branch `v3/pr9-windows`.

## Scope: PR 9, Windows CI leg and cwd digest portability (D36, revised)

Finding from your first pass, confirmed by the controller: the bug the CI comment describes and its two named tests were deleted in the planner v2 rewrite (`d9ee701`). The comment is stale. The revised scope is:

1. In `.github/workflows/ci.yml`, add `windows-latest` to the test matrix and delete the stale comment. The Windows check on your PR is the proof the old bug is gone.
2. Fix the real portability issue you found: pane and workspace `cwd` enter the digest content in `src/ir.rs` as a `PathBuf`, so the same declared path digests differently per OS. Add a helper in a new `src/paths.rs` that renders a path in one stable form on every OS (forward slashes, built from `Path::components`, no string replacement of `/`), and use it at the one place in `src/ir.rs` where `cwd` is put into the digest JSON. That one call is your only change to `src/ir.rs`; PR 11 rewrites that file and rebases onto you.
3. Tests: (a) a Windows-style path (`C:\\repo\\sub`) and a Unix path with the same components render identically; (b) on Unix, every existing digest test vector is unchanged (D30 requires content digests to stay stable), so record the current digest of `examples/basic` before your change and assert it after.
4. Verify: `cargo test` locally; the PR's `windows-latest` leg is green.

Claimed paths: `src/paths.rs`, `src/main.rs` or `src/lib.rs` (the one `mod paths;` line), `src/ir.rs` (one call), `.github/workflows/ci.yml`.

Out of scope: `src/planner.rs`, everything else. If the Windows leg fails for a reason outside these files, report `BLOCKED` with the failing test names.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
