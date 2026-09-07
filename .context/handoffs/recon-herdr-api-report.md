# Herdr socket API recon (vs Drove)

**Herdr:** 0.8.2 · protocol 20 · schema in scratch dir · **Drove:** `src/herdr.rs`

---

## 1. Full API method table

Request: `{"id","method","params"}` → `{"id","result"}` or `{"id","error":{code,message}}`.

| Method | Required params | Optional params | Result (one line) |
|--------|-----------------|-----------------|-------------------|
| `ping` | — | — | `{type:"pong", version, protocol, capabilities?}` |
| `session.snapshot` | — | — | `{type:"session_snapshot", snapshot: SessionSnapshot}` |
| `workspace.create` | — | cwd, env, focus, label | `{type:"workspace_created", workspace, tab, root_pane}` |
| `workspace.list` | — | — | `{type:"workspace_list", workspaces[]}` |
| `workspace.get` | workspace_id | — | `{type:"workspace_info", workspace}` |
| `workspace.focus` | workspace_id | — | `{type:"ok"}` |
| `workspace.rename` | workspace_id, label | — | `{type:"ok"}` |
| `workspace.move` | workspace_id, insert_index | — | `{type:"ok"}` |
| `workspace.move_block` | workspace_ids | before_workspace_id | `{type:"ok"}` |
| `workspace.close` | workspace_id | — | `{type:"ok"}` |
| `workspace.report_metadata` | workspace_id, source, tokens | seq, ttl_ms | `{type:"ok"}` |
| `worktree.create` | — | base, branch, cwd, focus, label, path, workspace_id | `{type:"worktree_created", workspace, tab, root_pane, worktree}` |
| `worktree.open` | — | branch, cwd, focus, label, path, workspace_id | `{type:"worktree_opened", workspace, tab, root_pane, worktree, already_open}` |
| `worktree.list` | — | cwd, workspace_id | `{type:"worktree_list", source, worktrees[]}` |
| `worktree.remove` | workspace_id | force | `{type:"worktree_removed", workspace_id, path, forced}` |
| `tab.create` | — | cwd, env, focus, label, workspace_id | `{type:"tab_created", tab, root_pane}` |
| `tab.list` | — | workspace_id | `{type:"tab_list", tabs[]}` |
| `tab.get` | tab_id | — | `{type:"tab_info", tab}` |
| `tab.focus` | tab_id | — | `{type:"ok"}` |
| `tab.rename` | tab_id, label | — | `{type:"ok"}` |
| `tab.move` | tab_id, insert_index | — | `{type:"ok"}` |
| `tab.close` | tab_id | — | `{type:"ok"}` |
| `layout.export` | — | pane_id, tab_id | `{type:"layout_export", layout: LayoutDescription}` |
| `layout.apply` | root | focus, tab_id, tab_label, workspace_id | `{type:"layout_apply", layout: LayoutDescription}` |
| `layout.set_split_ratio` | path, ratio | pane_id, tab_id | `{type:"layout_split_ratio_set", layout}` |
| `pane.list` | — | workspace_id | `{type:"pane_list", panes[]}` |
| `pane.get` | pane_id | — | `{type:"pane_info", pane}` |
| `pane.current` | — | caller_pane_id | `{type:"pane_current", pane}` |
| `pane.layout` | — | pane_id | `{type:"pane_layout", layout}` |
| `pane.split` | direction | cwd, env, focus, ratio, right_click, target_pane_id, workspace_id | `{type:"pane_info", pane}` (new pane) |
| `pane.move` | pane_id, destination | focus | `{type:"pane_move", move_result}` |
| `pane.swap` | — | direction, pane_id, source_pane_id, target_pane_id | `{type:"pane_swap", swap}` |
| `pane.focus` | pane_id | — | `{type:"ok"}` |
| `pane.focus_direction` | direction | pane_id | `{type:"pane_focus_direction", focus}` |
| `pane.neighbor` | direction | pane_id | `{type:"pane_neighbor", neighbor}` |
| `pane.edges` | — | pane_id | `{type:"pane_edges", edges}` |
| `pane.resize` | direction | amount, pane_id | `{type:"pane_resize", resize}` |
| `pane.zoom` | — | mode, pane_id | `{type:"pane_zoom", zoom}` |
| `pane.rename` | pane_id | label | `{type:"ok"}` |
| `pane.close` | pane_id | — | `{type:"ok"}` |
| `pane.read` | pane_id, source | format, lines, strip_ansi | `{type:"pane_read", read}` |
| `pane.wait_for_output` | pane_id, source, match | lines, strip_ansi, timeout_ms | `{type:"output_matched", ...}` |
| `pane.send_text` | pane_id, text | — | `{type:"ok"}` |
| `pane.send_keys` | pane_id, keys | — | `{type:"ok"}` |
| `pane.send_input` | pane_id | keys, text | `{type:"ok"}` |
| `pane.process_info` | — | pane_id | `{type:"pane_process_info", process_info}` |
| `pane.input.set` | pane_id, right_click | — | `{type:"ok"}` |
| `pane.report_metadata` | pane_id, source | title, tokens, state_labels, display_agent, agent, ttl_ms, seq, clear_* | `{type:"ok"}` |
| `pane.report_agent` | pane_id, source, agent, state | message, agent_session_id/path, seq | `{type:"ok"}` |
| `pane.report_agent_session` | pane_id, source, agent | agent_session_id/path, seq, session_start_source | `{type:"ok"}` |
| `pane.release_agent` | pane_id, source, agent | seq | `{type:"ok"}` |
| `pane.clear_agent_authority` | pane_id | seq, source | `{type:"ok"}` |
| `pane.graphics.{set,clear,info}` | pane_id (+ image fields for set) | layer_id, placement, … | graphics overlay |
| `agent.start` | name, kind, pane_id | args, timeout_ms | `{type:"agent_started", agent, argv}` |
| `agent.list` | — | — | `{type:"agent_list", agents[]}` |
| `agent.get` | target | — | `{type:"agent_info", agent}` |
| `agent.prompt` | target, text | wait:{until[], timeout_ms?} | `{type:"agent_prompted", agent}` |
| `agent.wait` | target | until[], timeout_ms | `{type:"agent_info", agent}` |
| `agent.read` | target, source | format, lines, strip_ansi | read payload |
| `agent.send_keys` | target, keys | — | `{type:"ok"}` |
| `agent.rename` | target | name | `{type:"ok"}` |
| `agent.focus` | target | — | `{type:"ok"}` |
| `agent.explain` | target | — | detection explain payload |
| `agent.view.set/clear` | source (set) | filter, label, sort / source | agent view state |
| `events.subscribe` | subscriptions[] | — | `{type:"ok"}` |
| `events.wait` | match_event | timeout_ms | subscription event envelope |
| `notification.show` | title | body, position, sound | `{type:"notification_show", shown, reason}` |
| `server.agent_manifests` | — | — | `{manifests[]}` (installed agent kinds) |
| `server.live_handoff` | — | expected_protocol, expected_version, import_exe | live handoff result |
| `server.reload_config` | — | — | `{type:"ok"}` |
| `server.reload_agent_manifests` | — | — | `{type:"ok"}` |
| `server.stop` | — | — | `{type:"ok"}` |
| `client.window_title.set/clear` | title (set) | — | window title result |
| `integration.install/uninstall` | target | — | install/uninstall messages |
| `plugin.{link,list,enable,disable,unlink,pane.open,action.*,log.list}` | varies | per subcommand | plugin mgmt |
| `popup.close` | — | — | `{type:"ok"}` |

**CLI-only:** `herdr pane run` — send-text+Enter; no dedicated socket method.

---

## 2. What Drove calls today

| Drove wrapper | Socket method | Params Drove sends |
|---------------|---------------|-------------------|
| `ping()` | `ping` | `{}` |
| `snapshot()` | `session.snapshot` | `{}` → reads `.result.snapshot` |
| `export_layout(tab_id)` | `layout.export` | `{tab_id}` |
| `create_workspace(label, cwd)` | `workspace.create` | `{label, cwd, focus:false}` — **no env** |
| `apply_layout(...)` | `layout.apply` | `{tab_label, focus:false, root}` plus either `{tab_id}` (replace) or `{workspace_id}` (new tab) |
| `rename_workspace` | `workspace.rename` | `{workspace_id, label}` |
| `rename_tab` | `tab.rename` | `{tab_id, label}` |
| `start_agent` | `agent.start` | `{pane_id, name, kind, args}` — **no timeout_ms, no prompt** |
| `report_workspace_status` | `workspace.report_metadata` | `{workspace_id, source:"drove", tokens:{drove_status: status}}` |

**Layout `root` (`model.rs`):** pane: `cwd`, `label?`, `command?`, `env?`; split: `direction`, `ratio`, `first`, `second`. Drove logical pane IDs are mapped post-apply from returned runtime IDs.

**Executor:** create workspace → snapshot for initial tab → `layout.apply` per tab (first replaces default) → `agent.start` → `report_metadata`.

**Socket discovery (`resolve_socket_path`):** explicit path > `--session` > `HERDR_SOCKET_PATH` > `HERDR_SESSION` > default `~/.config/herdr/herdr.sock`.

---

## 3. Capability deep dives

### Workspace create (cwd, label, env)

```json
// workspace.create params (all optional)
{"label": "control", "cwd": "/path/to/repo", "env": {"FOO": "bar"}, "focus": false}

// result
{"type": "workspace_created", "workspace": WorkspaceInfo, "tab": TabInfo, "root_pane": PaneInfo}
```

Drove passes `label`, `cwd`, `focus:false` only. Herdr also accepts workspace-level `env`.

### Layout apply / export

**Apply** — one call replaces or creates a full tab tree:

```json
// layout.apply params
{
  "workspace_id": "w1",          // OR tab_id to replace existing tab
  "tab_label": "coordinator",
  "focus": false,
  "root": {
    "type": "split", "direction": "down", "ratio": 0.5,
    "first": {"type": "pane", "cwd": "/repo", "label": "shell", "command": ["npm", "run", "dev"], "env": {"PORT": "3000"}},
    "second": {"type": "pane", "cwd": "/repo/sub"}
  }
}
```

`LayoutNode` pane fields: `command` (argv array), `cwd`, `env`, `label`, optional `pane_id` (hint for reuse). Split fields: `direction` (`right`|`down`), `ratio` (float), nested `first`/`second`.

**Export** — live `layout.export` on `w1:t1`:

```json
{
  "workspace_id": "w1", "tab_id": "w1:t1", "zoomed": false, "focused_pane_id": "w1:p1",
  "root": {
    "type": "split", "direction": "down", "ratio": 0.5,
    "first": {"type": "pane", "pane_id": "w1:p1", "cwd": "/Users/jjmartin/Development/Drove"},
    "second": {"type": "pane", "pane_id": "w1:p2", "label": "eventlog", "cwd": "/Users/jjmartin/Development/Drove"}
  }
}
```

**Export omits:** `command`, `env`, agent name/kind/status. **Export includes:** `pane_id`, `cwd`, `label` (if set), split geometry. Agent info lives on `session.snapshot` panes/agents arrays, not in layout export.

Drove compares export to desired JSON stripping `pane_id` — cannot detect command/env drift via export alone. **Replace destroys PTYs** (Drove `--allow-replace`).

### Command in pane vs starting a pane with a command

| Mechanism | API | Behavior |
|-----------|-----|----------|
| Declarative | `layout.apply` pane node `command: ["argv"...]` | Spawns process when pane is created/replaced |
| Imperative split | `pane.split` + `pane.send_input`/`send_text` | Split creates empty shell pane; send runs command |
| CLI sugar | `herdr pane run <id> "cmd"` | Atomic text+Enter into existing pane |
| Agent | `agent.start` | Requires shell at prompt; launches agent binary; **no command argv for arbitrary processes** |

When a pane `command` exits, Herdr removes the pane (Drove README); Drove treats that as layout drift.

### Agent start / prompt / wait

**`agent.start` params:**

```json
{"pane_id": "w1:p2", "name": "reviewer", "kind": "cursor", "args": ["--model", "x"], "timeout_ms": 30000}
```

- **Kinds** (CLI `--kind`): pi, claude, codex, gemini, cursor, devin, agy, cline, omp, mastracode, opencode, copilot, kimi, kiro, droid, amp, grok, hermes, kilo, qodercli, qwen, maki (20 manifests installed here).
- **No initial prompt** in `agent.start`. Send text via separate `agent.prompt`.
- **`timeout_ms`:** 3000–300000; default 30000. Returns after agent detected + interactive-ready.
- **Result:** `{type:"agent_started", agent: AgentInfo, argv: [...]}`.

**`agent.prompt`:**

```json
{"target": "reviewer", "text": "fix the bug", "wait": {"until": ["idle"], "timeout_ms": 60000}}
```

- `target`: unique agent **name** or pane ID hosting that agent.
- **`wait` optional:** blocks until status in `until` (enum: idle, working, blocked, done, unknown). Default after `--wait`: idle, done, or blocked.
- **Stall guard:** if not already working, requires state change within 5000ms or returns `agent_prompt_stalled`.
- **Blocked agents:** prompt rejected with `agent_blocked` before input sent.
- Without `timeout_ms`, wait is indefinite.

**`agent.wait`:** same `until`/`timeout_ms` semantics without sending text.

### Worktree create / open

```json
// worktree.create (all optional)
{"path": ".worktrees/feature", "branch": "feature/x", "base": "main", "cwd": "/repo", "label": "feature-x", "workspace_id": "w1", "focus": false}

// worktree.open
{"path": ".worktrees/feature", "branch": "feature/x", "cwd": "/repo", "label": "...", "workspace_id": "w1", "focus": false}
```

Creates/opens a git worktree **and** a Herdr workspace+tab+root pane bound to it. `worktree.list` returns repo metadata + worktrees with `open_workspace_id`. `worktree.remove` takes `workspace_id`, optional `force`.

### report-metadata (tokens, titles)

**Workspace** — display-only sidebar tokens (max 16 keys, `[A-Za-z0-9_-]{1,32}`):

```json
{"workspace_id": "w5", "source": "drove", "tokens": {"drove_status": "in sync"}, "seq": null, "ttl_ms": null}
→ {"type": "ok"}
```

Tokens appear on `session.snapshot` workspaces (verified: `w5.tokens.drove_status`). Event: `workspace.metadata_updated`.

**Pane** — richer overlay:

```json
{"pane_id": "w5:p1", "source": "drove", "title": "Test Title", "tokens": {"foo": "bar"},
 "state_labels": {}, "display_agent": null, "ttl_ms": null}
```

Also supports `clear_title`, `clear_state_labels`, `clear_display_agent`, `applies_to_source`. Verified: `title` and `tokens` appear on snapshot panes; triggers `pane.updated`.

### Notifications

```json
{"title": "Drove", "body": "Drift detected", "position": "top-right", "sound": "done"}
→ {"type": "notification_show", "shown": true|false, "reason": "shown"|"disabled"|"rate_limited"|"no_foreground_client"|"busy"}
```

Requires foreground Herdr client for `shown:true`.

### Caller's pane (`HERDR_PANE_ID`)

Herdr injects into managed panes: `HERDR_ENV=1`, `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID`, `HERDR_SOCKET_PATH`.

| Need | API |
|------|-----|
| Resolve caller | `pane.current` with `caller_pane_id` or CLI `--current` |
| Relabel | `pane.rename` `{pane_id, label}` |
| Move to tab | `pane.move` destination `{type:"tab", tab_id, split, target_pane_id?, ratio?}` |
| Move to new tab | `{type:"new_tab", workspace_id?, label?}` |
| Move to new workspace | `{type:"new_workspace", label?, tab_label?}` |
| Adopt external pane | **No adopt API.** Only move/rename existing Herdr panes. Processes outside Herdr cannot be imported. |

After cross-workspace move, pane gets **new** `pane_id`; old ID only resolves inside the moved process's inherited env.

### Move / close

- **`pane.move`:** returns `move_result` with `pane`, `previous_pane_id`, `previous_tab_id`, `previous_workspace_id`, optional `created_tab`/`created_workspace`, `closed_tab_id`, `closed_workspace_id`.
- **`pane.close`**, **`tab.close`**, **`workspace.close`:** `{type:"ok"}`. Closed IDs never reused.
- **`tab.move`**, **`workspace.move`**, **`workspace.move_block`:** reorder sidebars.

### Named sessions and `--remote`

- **`herdr --session NAME`:** socket at `~/.config/herdr/sessions/NAME/herdr.sock`.
- **`herdr --remote <ssh-target>`:** SSH attach; `--handoff` → `server.live_handoff`.

---

## 4. Useful Herdr capabilities Drove does not use (ranked)

1. **`agent.prompt` + `agent.wait`** — send initial/task prompts and block on agent state (Drove only starts agents).
2. **`workspace.create` / `tab.create` env`** — propagate env vars at workspace/tab scope without per-pane layout.
3. **`pane.report_metadata`** — pane titles, state_labels, display_agent for sidebar UX beyond workspace tokens.
4. **`events.subscribe` / `events.wait`** — react to pane/agent/workspace lifecycle without polling snapshot.
5. **`worktree.create` / `worktree.open`** — git-worktree-backed workspaces in one call.
6. **`tab.create`** — add tabs without full layout replace (avoids PTY destruction).
7. **`layout.set_split_ratio`** — adjust ratios without replacing entire tab.
8. **`notification.show`** — surface drift/ completion to user when no terminal focused.
9. **`pane.wait_for_output` / `pane.read`** — health checks on long-running pane commands.
10. **`server.agent_manifests`** — discover agent kinds at runtime.

## 5. Constraints and gotchas

- **ID stability:** closed IDs never reused; cross-workspace move → new `pane_id` (use `move_result.pane.pane_id`).
- **Layout replace:** `layout.apply` on existing `tab_id` destroys PTYs/scrollback/processes (Drove `--allow-replace`).
- **Export blind spot:** no `command`/`env` in export; Drove drift check misses command/env changes.
- **Agent start:** blocks until detection (default 30s); `agent_not_ready` if blocked at startup.
- **Agent prompt:** 5s stall guard; `agent_blocked` pre-send; no timeout = indefinite wait.
- **Pane command exit:** Herdr removes pane → Drove reports layout drift.
- **Agent.start** requires shell at prompt; never creates/splits/moves layout.
- **Focus:** bare targets may hit another client's focused pane; use explicit ID or `--current`.
- **Notifications:** `no_foreground_client` when no UI attached.
- **Protocol 20** required (Herdr 0.8.2 tested).

*Schema:* scratch dir `schema.json` from `herdr api schema --json`.
