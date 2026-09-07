# recon-radiator-gaps report

Read-only survey of `~/Development/radiator-cli` hub (Sep 2026). Answers what Drove v2 §3/§9 needs from the Radiator backend capability matrix.

---

## Capability table

| # | Need (Drove v2) | Status | Evidence | Smallest hub-side addition if missing/partial |
|---|---|---|---|---|
| 1 | Per-pane metadata tokens in snapshot (`drove_name`, `drove_profile`, `drove_digest`) | **missing** | `PaneInfo` is `{id, kind, title, runner}` only — `crates/proto/src/types.rs:179-184`; returned by `hub.snapshot` — `crates/hub/src/server.rs:684-686`. No `drove_*` or generic metadata anywhere. Design §9 D21 expects these tokens. | Add `metadata: HashMap<String,String>` to `PaneInfo` (and optionally `WorkspaceInfo`). New RPC `pane.set_metadata` / `workspace.set_metadata` with `{id, metadata}` (merge or replace). Drove writes three keys: `drove_name`, `drove_profile`, `drove_digest`. |
| 2 | Read recent pane output for `ready=output("…")` | **partial** | `pane.read` → `ScreenSnapshot` (visible grid only) — `crates/hub/src/server.rs:1085-1093`, `crates/proto/src/types.rs:252-258`. `pane_output` is signal-only — `docs/HUB-PROTOCOL.md:80-88`. Emulator has **no scrollback** — `crates/hub/src/emulator.rs:47-50`. | Add `pane.tail` with `{id, lines?: u32, match?: string}` returning `{lines: [string], matched: bool}`. Internally: small scrollback ring (e.g. 500 lines) or scan visible grid + ring on each PTY burst. D23 marks `output()` unsupported until this exists; planner can gate on capability flag `readiness_output`. |
| 3 | Process info: foreground pid, argv, exited-or-running | **partial** | Internal: `TermPane::foreground_pid()` — `crates/hub/src/pane_term.rs:156-164`; poll uses process **name** not argv — `crates/hub/src/server.rs:503-545`. Wire: `PaneInfo` / snapshot carry none of this. Exit only via `pane_exited` event — `crates/proto/src/event.rs:59-65`. Spawn argv known at `pane.open` (`OpenPaneParams.command/args` — `crates/hub/src/server.rs:572-576`) but not stored on snapshot. | Extend `PaneInfo` with optional `process: {pid, argv, status}` where `status` is `"running"|"exited"` and `argv` is spawn-time `[command, ...args]`. Populate on snapshot from `TermPane` + stored spawn spec. No new RPC required if snapshot is enough; optional `pane.process` for on-demand refresh. |
| 4 | Close pane, rename pane, rename workspace | **partial** | Close pane: `pane.close` — `crates/hub/src/server.rs:810-814`. Rename pane: `pane.rename` — `728-741`, emits `PaneRenamed` — `crates/hub/src/registry.rs:183-194`. Close workspace: `workspace.close` — `715-726`. **Rename workspace: missing** — no `workspace.rename`, `Command` enum has no variant — `crates/hub/src/registry.rs:24-54`; CLI `WorkspaceCmd` is Open/Close/List only — `src/cli.rs:155-163`. | Add `workspace.rename` with `{id, name}` → updates `WorkspaceInfo.name`, emits `workspace_renamed {seq, id, name}`. |
| 5 | Start coding agent in chat pane + initial prompt + idle detection | **partial** | `pane.open` with `kind: "chat"` spawns in-process **mock** agent — `crates/hub/src/server.rs:897-938`, `crates/hub/src/pane_chat.rs:79-91`. No `prompt`/`message` on open; prompt via separate `chat.send` — `crates/hub/src/server.rs:1177-1189`. Term panes: `pane.open` accepts `command`, `args`, `env` (e.g. `RADIATOR_PROMPT`) — `566-589`; Drove pattern is run agent binary as `serve`. Idle: `RunnerState` on snapshot (`idle|working|…`) — `crates/proto/src/types.rs:179-184`; chat transcript has `in_flight` — `crates/hub/src/pane_chat.rs:48-52`; term poll ~2s on process name — `crates/hub/src/server.rs:44-48`, `crates/hub/src/runner.rs:53`. No dedicated “agent idle” for real CLIs beyond poll + `runner.set_state` hooks. | For term agents (Drove D7): no hub change — Drove uses `pane.open` with agent argv + `pane.send_text` after attach (already exists). Optional: `pane.open` field `initial_text` (auto `send_text` after spawn). For chat: add `message` to `pane.open` when `kind=chat`, or document `chat.send` immediately after open. Idle: expose `chat.state.in_flight` in snapshot `PaneInfo` or rely on `runner_state` events + `ChatEvent` `done` payload. |
| 6 | Stable pane identity across hub restarts (`w1:p2` survives reconnect?) | **partial** | **Client reconnect to live hub: yes** — ids stable, detach does not kill panes — `docs/HUB-PROTOCOL.md:23-25`. **Hub process restart: no** — persistence exports layout shape without ids — `crates/hub/src/registry.rs:236-238`; restore allocates fresh ids — `258-259`, `154-155`; docs confirm — `docs/HUB-PROTOCOL.md:186-191`. | Persist `{ws_counter, pane_counters, stable_id → PaneId}` in state file, or add optional `external_id: string` on `LayoutPane` / `pane.open` that is re-used on restore. Smallest: store `drove_name` in layout export and remap to new numeric id on load (requires item 1 metadata). |
| 7 | `events.subscribe` on pane open/close/exit + example payload | **exists** | Subscribe returns snapshot then stream — `crates/hub/src/server.rs:361-367`. Variants — `crates/proto/src/event.rs:25-65`. `pane.close` on term emits **both** `pane_exited` then `pane_closed` — `crates/hub/src/server.rs:1013-1028`. | None required. |

### Example event payloads (wire JSON)

```json
{"event":"pane_opened","seq":2,"workspace":"w0","pane":{"id":"w0:p1","kind":"term","title":"gitlog","runner":"idle"}}
{"event":"pane_exited","seq":8,"id":"w0:p1","exit_code":0}
{"event":"pane_closed","seq":9,"id":"w0:p1"}
```

---

## Proposed protocol sketch (hub additions for Drove PR 5)

Minimal delta on existing Herdr-shaped hub protocol (`docs/HUB-PROTOCOL.md`):

```
# Metadata (D21 ownership tokens)
pane.set_metadata   {id, set: {drove_name, drove_profile, drove_digest, ...}}
workspace.set_metadata {id, set: {...}}   # optional; drove_repo on workspace if needed

# Snapshot shape change
PaneInfo += metadata: object<string,string>
PaneInfo += process?: {pid: int|null, argv: [string], status: "running"|"exited", exit_code?: int|null}

# Readiness (D23 output probe)
pane.tail           {id, lines?: int, match?: string} → {lines: [string], matched: bool}
hub.capabilities    → {readiness_output: bool, metadata: bool, ...}   # or static in docs

# Workspace rename (planner RenameWorkspace)
workspace.rename    {id, name} → WorkspaceInfo; event workspace_renamed

# Optional ergonomics
pane.open           + initial_text?: string   # post-spawn send_text once
layout export/import + external_id?: string per pane   # stable id across hub restart
```

**Drove backend degradation (unchanged design intent):** tabs/splits ignored; agent start = `pane.open` + `send_text`; `ready=output()` blocked until `pane.tail` + capability flag; drift via metadata digest compare first (item 1), process argv second (item 3); workspace rename skipped with warning until item 4 lands.

**Priority for hub PR before Drove PR 5:** (1) metadata tokens, (3) process on snapshot, (2) pane.tail, (4) workspace.rename, (6) external_id persistence — in that order for unblock value.
