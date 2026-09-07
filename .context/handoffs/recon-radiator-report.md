# recon-radiator report

Read-only survey of `~/Development/radiator-cli`, `~/Development/radiator-neue`, `~/Development/radiator-engineering`, and skim of `~/Development/radiator-std-snapshot` (legacy k8s dump only — no layout model). Sep 2026.

---

## 1. What Radiator is and its current state

**Radiator** (per `radiator-engineering/CONTEXT.md`) is a spoken interface to automated work: a person talks to an **aide**, which coordinates **tasks** and **turns** run on enrolled **boxes**, with the **plane** as authority and record. Engineering is one use case, not the whole product.

In practice Radiator is **several repos at different maturity levels**, not one binary:

| Repo | Role | State |
|---|---|---|
| **`radiator-cli`** | Local **hub daemon** (owns PTYs + server-side terminal emulator), thin **attach TUI**, one-shot **CLI** over a Unix socket, optional **box enrollment** to a plane. Deliberately Herdr-shaped ids (`w0`, `w0:p1`). | **Active prototype, in local use.** Recent commits (Sep 2026) add clickable TUI, cursor rendering. README positions hub as opt-in; bare `radiator` can auto-start hub + attach. |
| **`radiator-neue`** (package name `manifold`) | Browser **desktop OS on an infinite canvas**: React Flow windows, view registry, Zustand + intent bus. | **UI prototype only.** No backend, no auth, no agent runtime. Persists to `localStorage`. |
| **`radiator-engineering`** | Production stack: **plane** (Go), **frontdoor** (gateway + Python voice agent), **lifecycle** (box turn runner), **stdb** (SpacetimeDB canvas wasm), **infra**, Swift client. | **Under active development**, not finished. Eight CI checks gate merge; credential path box→gateway not proven E2E against real gateway (`README.md`). |
| **`radiator-std-snapshot`** | Frozen GKE YAML (Dapr agent mesh, Jul 2026). | **Archive only** — no layout model. |

No repo contains **Drove** / **Drovefile** generation today; that goal is **aspirational** in the code surveyed.

---

## 2. Layout model and Herdr mapping

### 2a. Nouns by surface

**`radiator-cli` (terminal hub — closest Herdr analogue)**

```
Hub (named daemon, default "main")
 └── Workspace (named group, id w{n})
      └── Pane (id w{ws}:p{n}, kind term|chat)
           ├── term: PTY + server-side emulator (process keeps running when TUI detaches)
           └── chat: in-process mock agent; streams ServerEvents
```

- **No tab layer.** Workspaces are flat lists of panes; TUI sidebar shows `SPACES` tree (workspace → panes) plus an **ATTENTION** queue and an **agents** band (chat panes).
- **No spatial splits.** `LayoutDoc` is a flat list per workspace; comments in `crates/proto/src/types.rs` say pane *tree* (splits) is deferred.
- **Runner state** per pane (`working|blocked|done|idle|unknown`) rolls up to workspace; hooks via `runner.set_state` beat a ~2s process poll.
- **Fleet** (`crates/tui/src/fleet.rs`) — read-only console list from plane; not wired to hub tree yet.

**`radiator-neue` (manifold)**

```
Desktop (one canvas / React Flow graph; multi-desktop planned)
 ├── Window (React Flow node, lifecycle: windowed|maximized|minimized)
 │    └── View (plugin: vault, tasks, decisions, agents, settings, demo)
 ├── CanvasObject (sticky, shape, …)
 ├── Connector (edge)
 └── Viewport (pan/zoom — local; collab transport stub)
```

Mutations: single `dispatch(AgentIntent)` choke point (`src/state/intents/dispatch.ts`).

**`radiator-engineering` (distributed)**

```
Org
 ├── Plane (tasks, turns, decisions, boards, fleet spend, …)
 ├── Box (enrolled machine; holds signing key)
 │    ├── Hub (radiator-cli daemon on the box)
 │    │    └── workspaces / panes (local)
 │    └── Published Console (box → plane via POST /machine/v1/consoles)
 └── Canvas (SpacetimeDB per org: radiator-canvas-org-{slug})
      ├── Node (kind, label, x/y/w/h/z, item_id → plane asset)
      ├── Edge (connectors)
      └── Viewport (per-connection pan/zoom)
```

Plane **does not** store “shape of the work” on a box (ADR 0009); local terminal layout stays on the box.

### 2b. Herdr → Radiator mapping

| Herdr | `radiator-cli` hub | `radiator-neue` | `radiator-engineering` |
|---|---|---|---|
| **workspace** | **Workspace** (`w{n}`, named) | **Desktop** (canvas instance) | **Org-scoped context**; local hub workspaces are not plane objects |
| **tab** | **none** (no intermediate layer) | **none** (windows sit on desktop directly) | **none** at terminal layer; **Board** on plane is a different concept (task/decision surface) |
| **pane** | **Pane** (`w{ws}:p{n}`, `term` or `chat`) | **Window** hosting a View | **Console** (published surface) maps to a local pane; plane sees metadata not PTY |
| **agent** | **Chat pane** + **runner state** + optional hook to agent CLIs in **term panes**; no `agent start` RPC | **Participant** / Agents view (demo data); intents, no runtime | **Turn** on a **box** via lifecycle; **aide** at frontdoor; not a hub pane type |
| **worktree** | **none** | **none** | **none** |

**Closest Drove backend target:** `radiator-cli` hub socket protocol — ids, layout export/apply, pane run/send-keys, runner hooks.

---

## 3. Radiator-only concepts a shared Drove model must accommodate

1. **Hub as process owner** — PTYs and emulator grids live in the daemon; clients are mirrors. Detach ≠ stop. Drove “reconcile” must treat hub lifetime separately from TUI attach.
2. **Runner state + attention queue** — `blocked`/`done` are hook-only; `pane seen` acks `done→idle`. No Herdr `agent prompt` equivalent on the hub wire.
3. **Chat panes** — non-terminal pane kind with `chat.send` / `chat.state` and SSE-shaped `chat_event`s folded through `radiator_chat::reduce`.
4. **Server-side terminal emulator** — `pane.read` returns `ScreenSnapshot` (plain text lines); `pane_output` events are signals only.
5. **Box enrollment & plane tasks** — supervisor `serve_one` claims `turn|console|command` tasks and opens panes via `JobHost`; work is plane-driven, not declarative-local.
6. **Consoles (upstream protocol)** — box publishes console snapshots/beats to plane (`docs/PROTOCOL-CONSOLES.md`); vocabulary intentionally not “pane” on the wire.
7. **Remote fleet view** — consoles aggregated cross-box (TUI fleet mode); not the same object as hub workspaces.
8. **Canvas / board (stdb)** — spatial graph with `item_id` join to plane assets; shared visibility (minimize is global); JWT-scoped SpacetimeDB per org.
9. **Non-terminal panels (frontdoor voice UI)** — grid layout with typed panels (`chart`, `data`, `markdown`, `iframe`, …) mutated server-side via JSON Patch through `SharedStateContainer` — orthogonal to hub `LayoutDoc`.
10. **Intent bus (manifold)** — every UI mutation is an `AgentIntent`; future collab sync expects this shape.
11. **Privacy tiers on console beats** — `Presence|Beats|Scrollback` gates what leaves the box (`crates/consoles/src/publisher.rs`).
12. **Layout persistence gap** — `LayoutDoc` stores only `kind` + `title`; **no cwd, command, env, or split geometry**. Restore spawns fresh shells in hub cwd.

---

## 4. Config / manifest formats (with excerpts)

### Hub layout document (`LayoutDoc`)

Persisted at `$XDG_RUNTIME_DIR/radiator/hub-{name}.layout.json` (fallback `~/.local/state/radiator/…`). Types in `crates/proto/src/types.rs`.

```json
{
  "workspaces": [
    {
      "name": "dev",
      "panes": [
        { "kind": "term", "title": "shell" },
        { "kind": "chat", "title": "aide" }
      ]
    }
  ]
}
```

CLI: `radiator layout export` → `layout.export`; `radiator layout apply layout.json` → `layout.apply`.

### Hub RPC envelope (NDJSON on Unix socket)

`docs/HUB-PROTOCOL.md` — default socket `$XDG_RUNTIME_DIR/radiator/hub-main.sock`.

Request:
```json
{"id": 1, "method": "workspace.open", "params": {"name": "dev"}}
```

`pane.open` (implementation also accepts `args`, `cwd`, `rows`, `cols` — see `OpenPaneParams` in `crates/hub/src/server.rs`; protocol doc lags slightly):
```json
{
  "id": 2,
  "method": "pane.open",
  "params": {
    "workspace": "w0",
    "kind": "term",
    "title": "shell",
    "command": "bash",
    "args": ["-l"],
    "cwd": "/path/to/repo"
  }
}
```

Snapshot bootstrap:
```json
{"id": 3, "method": "hub.snapshot", "params": {}}
```
→ `{ "workspaces": [WorkspaceInfo…], "seq": 42 }`

### Voice-agent workspace layout (frontdoor, browser grid)

`frontdoor/agent/radiator_agent/workspace_tools.py` — `_LAYOUT_TEMPLATES["default"]`:

```json
{
  "mode": "grid",
  "cols": 12,
  "rowHeight": 30,
  "panels": [
    {"i": "transcript", "type": "transcript-log", "x": 0, "y": 0, "w": 3, "h": 24, "title": "Transcript"},
    {"i": "voice", "type": "voice-panel", "x": 3, "y": 0, "w": 6, "h": 24, "title": "Voice"},
    {"i": "actions", "type": "action-log", "x": 9, "y": 0, "w": 3, "h": 24, "title": "Actions"}
  ]
}
```

Mutated via JSON Patch ops on `/layout/panels/…` through `SharedStateContainer.apply`.

### Console snapshot (box → plane)

`docs/PROTOCOL-CONSOLES.md`:

```json
{
  "consoles": [{
    "console_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
    "title": "aide: fix flaky test",
    "kind": "aide",
    "state": "working",
    "task_id": "t_123",
    "turn_id": "u_456",
    "started_at": "2026-01-01T00:00:00Z",
    "state_changed_at": "2026-01-01T00:02:30Z",
    "last_beat_seq": 42,
    "scrollback_shared": false
  }],
  "snapshot_seq": 17
}
```

### Manifold persisted state

Browser `localStorage` via Zustand persist — windows, viewState, canvas objects. View plugin registry in `src/views/registry.ts` (`registerView({ id, title, defaultSize, component, createInitialState, … })`).

### stdb canvas node (SpacetimeDB)

`stdb/src/lib.rs` — `Node { kind, label, x, y, w, h, z, item_id, minimized, owner_person }`.

---

## 5. Server-side-constructed configurations — evidence

| What | Who generates | For whom | Status |
|---|---|---|---|
| **Hub layout** | Client via `layout.apply` or hub restore from `hub-main.layout.json` | Local CLI/TUI/supervisor on box | **Implemented** — shape only, no commands/cwd in doc |
| **Plane-driven panes** | **Supervisor** claims `console`/`command` tasks, calls hub `pane.open` with job `command`/`args`/`cwd`/`env` (`crates/supervisor/src/link.rs`, `ExecSpec` in tests) | Box executing plane work | **Implemented** in cli repo; E2E with real gateway **not proven** (engineering README) |
| **Console publication** | Box `Publisher` builds snapshots/beats | Plane + remote fleet UI | **Types + pure logic**; plane routes exist in engineering repo |
| **Voice UI grid** | Frontdoor agent tools (`add_panel`, `set_layout`, …) | Browser client in LiveKit room | **Implemented** for AG-UI panel grid, not terminal layout |
| **Canvas graph** | Gateway-minted tokens; clients call stdb reducers (`add_node`, …) | Org members on shared board | **Deployed module**; console is renderer |
| **Agent graph visualization** | Backend `AgentGraphState` snapshots (`plan_workspace` node in graph) | Frontdoor web UI | **Visualization**, not workspace provisioning |
| **Drovefile / Drove config** | — | Drove reconciler | **Not found** in any Radiator repo |

**Conclusion:** “Server-side-constructed configurations” exists today for (a) plane-assigned jobs opening hub panes, (b) voice-agent panel layouts, (c) collaborative canvas nodes. **No code generates a Drove declaration** yet; the natural seam would be either emitting `LayoutDoc` + follow-up `pane.open`/`pane.run` calls, or a new plane/frontdoor tool that writes Starlark/JSON for Drove.

---

## 6. Where a Drove backend would plug in

### Primary: `radiator-cli` hub socket (local)

**Transport:** NDJSON RPC on Unix domain socket (`radiator_hubclient::Client`).

**Lifecycle / layout**

| Operation | CLI | RPC method |
|---|---|---|
| Liveness | `radiator ping` | `hub.ping` |
| Full state | `radiator snapshot` | `hub.snapshot` |
| Shutdown | `radiator stop` | `hub.stop` |
| Open workspace | `radiator workspace open NAME` | `workspace.open {"name"}` |
| Close workspace | `radiator workspace close w0` | `workspace.close {"id"}` |
| List workspaces | `radiator workspace list` | `hub.snapshot` (derived) |
| Export layout | `radiator layout export` | `layout.export` |
| Apply layout | `radiator layout apply FILE` | `layout.apply {LayoutDoc}` |

**Panes / commands**

| Operation | CLI | RPC method |
|---|---|---|
| Open pane | `radiator pane open w0 [--kind term\|chat] [--title T] [--command C] [--args …] [--cwd D]` | `pane.open` |
| Close | `radiator pane close w0:p0` | `pane.close` |
| Rename | (RPC only in protocol) | `pane.rename` |
| Resize | `radiator pane resize w0:p0 ROWS COLS` | `pane.resize` |
| Read screen | `radiator pane read w0:p0` | `pane.read` → `ScreenSnapshot` |
| Send text/keys | `radiator pane send-text …`, `send-keys …` | `pane.send_text`, `pane.send_keys` |
| Run command | `radiator pane run w0:p0 "cmd"` | `pane.run` |
| Ack done | `radiator pane seen w0:p0` | `pane.seen` |
| Runner hook | `radiator runner set-state --state S [--pane ID]` | `runner.set_state` |
| Chat turn | — | `chat.send`, `chat.state` |

**Events:** `radiator events [--workspace …] [--pane …]` → `events.subscribe` with optional filter; monotonic `seq`.

**Env inside term panes:** `RADIATOR_HUB`, `RADIATOR_HUB_SOCKET`, `RADIATOR_PANE_ID`, `RADIATOR_WORKSPACE_ID` (`docs/HOOKS.md`).

**Daemon:** `radiator hub` (or implicit via bare `radiator` / attach).

### Secondary: plane / box path (remote orchestration)

- Enroll box: `radiator box join --code …` → boxwire to plane.
- Supervisor loop (box binary, not fully exposed as CLI verbs in survey): `claim` task → open pane on hub → publish console.
- Human auth: `radiator login`, `whoami`, `logout`.

Drove could treat **plane task dispatch** as an alternate backend for “start this command on my box” rather than talking to the hub directly — but that path is async, policy-gated, and not layout-declarative today.

### Not plug-in ready for terminal Drove

- **`radiator-neue`:** no API; would need new transport + intent→hub bridge.
- **Frontdoor `workspace_tools`:** AG-UI grid only.
- **stdb:** canvas graph, not shells.

---

## 7. Open questions

1. **Which Radiator surface is the Drove backend?** Hub socket is the only complete terminal CRUD API; plane/supervisor is task-shaped, not layout-shaped.
2. **Will Drove generation live in frontdoor (aide tool), plane, or hub?** No stub found; voice `set_layout` templates are unrelated to `LayoutDoc`.
3. **Tab/split geometry:** Hub explicitly deferred pane trees; can Drove map binary splits, or must Radiator gain splits first?
4. **LayoutDoc vs runtime pane spec:** Export omits `command`, `cwd`, `env`, `rows`/`cols`. Will Drove reconcile via `layout.apply` + per-pane `pane.open`, or will `LayoutDoc` grow?
5. **Agent model:** Herdr `agent start` / `agent prompt` have no hub equivalent — only `chat` panes and `runner.set_state`. How does Drove declare agentic workflows?
6. **Id stability:** `layout.apply` allocates **fresh** ids; no “ensure pane with logical name” RPC. Drove ownership-by-id may need a naming layer or hub changes.
7. **Multi-hub / remote:** Socket is local filesystem auth only. Cross-machine layouts imply box/plane/console path — unspecified for declarative reconcile.
8. **Fleet vs hub:** When does Drove target local hub vs published consoles on other boxes?
9. **Manifold vs cli convergence:** Will web canvas windows and terminal panes share one Drove vocabulary, or stay separate backends?
10. **Protocol doc drift:** `HUB-PROTOCOL.md` `pane.open` params omit `args`/`cwd` that `OpenPaneParams` and CLI already support — which doc is contract for Drove?

---

## Key files

`radiator-cli`: `README.md`, `docs/HUB-PROTOCOL.md`, `crates/proto/src/types.rs`, `crates/hub/src/server.rs`, `src/cli.rs`, `docs/PROTOCOL-CONSOLES.md`. `radiator-neue`: `README.md`, `docs/architecture-and-prd.md`. `radiator-engineering`: `README.md`, `CONTEXT.md`, `stdb/src/lib.rs`, `frontdoor/agent/radiator_agent/workspace_tools.py`.
