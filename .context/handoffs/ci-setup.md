# Brief: ci-setup (spawned PR worker, Opus 4.8)

You were spawned by the Drove controller to make this repo's CI real and green. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Plain commit messages, no attribution trailers.

Worktree: `~/Development/Drove-worktrees/ci-setup`, branch `chore/ci`. Remote: `git@github.com:radiator-engineering/Drove.git`; `gh` is authenticated.

## Situation
`.github/workflows/ci.yml` and `release.yml` were written during scaffolding and have never been checked against GitHub. Known doubts: `brew install herdr` may not exist (Herdr is a private/early tool; check `brew search herdr` and `~/Development` for how it is installed here); the smoke test in `scripts/smoke-herdr.sh` needs a Herdr binary; the Windows matrix leg may not build because of `interprocess` socket use; `cargo build --release --locked` needs `Cargo.lock` committed.

## Deliverable
1. A `ci.yml` that runs on `pull_request` and on `push` to `main`, with jobs: fmt, clippy (`-D warnings`), test on ubuntu and macos (drop Windows if it cannot pass; say why in the PR), `cargo doc --no-deps` with warnings denied, and `cargo audit` or `cargo deny check` (pick one, pin the action). Concurrency group per ref that cancels superseded runs. Pinned action versions.
2. The Herdr smoke job: keep it only if Herdr can be installed on a runner in a documented way; otherwise gate it behind `workflow_dispatch` and a self-hosted label, and leave a comment saying what is needed.
3. `release.yml` fixed to what actually works (add `Cargo.lock` if missing, a checksum step, and `softprops/action-gh-release` or equivalent to attach binaries to the tag).
4. A `Makefile` or `justfile` target `ci` that runs the same commands locally, and one line in `README.md` pointing at it.
5. Verify: push the branch, open the PR, watch `gh run watch` until every job is green, and fix until it is. Do not mark the PR ready until the run is green. Then set `main` branch protection to require the `ci` checks via `gh api` if the repo permissions allow; if not, say so in the PR body.

Final reply: the single line `PR READY: <url>` (only after a green run) or `BLOCKED: <one line>`.
