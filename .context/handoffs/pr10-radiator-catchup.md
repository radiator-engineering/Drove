# Brief: pr10-radiator-catchup (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Use TDD. Plain commit messages, no attribution trailers. When done, push and open the PR with `gh pr create --base main`; the body lists what changed, what is left for other PRs, and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must pass.

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v3-core-and-flavors-design.md`, decision **D35** and test §10 D35. Read it before writing anything. Do not change the `Backend` trait or `Capabilities` struct in `src/backend/mod.rs`; PR 11 owns those.

Worktree: `~/Development/Drove-worktrees/pr10-radiator-catchup`, branch `v3/pr10-radiator-catchup`.

## Scope: PR 10, Radiator backend reads the hub's real capabilities (D35)

Hub commit `80c0f1d` in `~/Development/radiator-cli` landed `pane.set_metadata`, `PaneInfo.metadata`, `PaneInfo.process`, `pane.tail`, `workspace.rename` and `hub.capabilities`. Read that commit and `~/Development/radiator-cli/docs/HUB-PROTOCOL.md` for the exact wire shapes. Then, in `src/backend/radiator.rs` only:

1. Call `hub.capabilities` when the client connects (or lazily on first use, cached). Set `metadata_tokens` and `process_info` in `capabilities()` from the reply instead of the hardcoded `false`.
2. Ownership tokens: when the hub reports metadata support, `pane.set_metadata` is authoritative and the local journal entry for that pane is dropped after a successful write. Keep the journal only for a hub that reports no metadata support. The existing doc comments in the file describe this intent; make the code match.
3. `process_info()` returns the hub's `PaneInfo.process` when present.
4. `rename_workspace()` calls `workspace.rename` and returns an error if the hub refuses, instead of the current warn-and-skip `bool`.
5. Fake-socket unit tests: a hub that answers `hub.capabilities` with metadata and process support, and one that answers without; assert the capability flags and the journal behavior in each case.

Keep every change inside existing method bodies, `capabilities()`, and one new capability-query helper. Do not rename, reorder or remove methods: PR 11 restructures this file's trait impl at the same time and rebases onto you; a body-only diff keeps that rebase clean.

Claimed paths: `src/backend/radiator.rs`.

Out of scope: `src/backend/mod.rs`, the Herdr backend, the planner, the CLI.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
