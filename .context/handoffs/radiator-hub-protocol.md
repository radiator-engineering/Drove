# Brief: radiator-hub-protocol (spawned PR worker, Sonnet) — repo ~/Development/radiator-cli

You were spawned by the Drove controller to add the hub protocol pieces Drove's Radiator backend needs. You work in `~/Development/radiator-cli` on branch `drove/hub-protocol` (create it from the current default branch; use a worktree at `~/Development/radiator-cli-worktrees/hub-protocol` if `git worktree add` works, otherwise the branch in place). Open a PR; do not merge. No attribution trailers. Do not touch `~/Development/Drove`.

Read first: `~/Development/Drove/.context/handoffs/recon-radiator-gaps-report.md` (the capability table cites the exact files and lines) and `docs/HUB-PROTOCOL.md` in the hub repo.

Implement, in this order, each as its own commit with tests:
1. `PaneInfo.metadata: HashMap<String,String>` plus RPC `pane.set_metadata {id, set}` (merge semantics) and `workspace.set_metadata`. Metadata survives layout export/import.
2. `PaneInfo.process: Option<{pid, argv, status: running|exited, exit_code}>` populated on snapshot from the spawn spec and the term pane.
3. `pane.tail {id, lines?, match?} -> {lines, matched}` backed by a 500-line scrollback ring in the emulator.
4. `workspace.rename {id, name}` with a `workspace_renamed` event.
5. `hub.capabilities -> {metadata: true, process: true, readiness_output: true, workspace_rename: true}`.
6. Update `docs/HUB-PROTOCOL.md` for each.

Run the repo's tests and lints the way its README or justfile says. Final reply: `PR READY: <url>` or `BLOCKED: <one line>`.

## After the PR opens
Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
