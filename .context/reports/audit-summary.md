# Audit summary: holes of the "trusted a stale picture" kind (2026-09-07)

Sources: `audit-state.md`, `audit-partial.md`, `audit-herdr.md`, `audit-planner.md`. PR 30 (D50) already covers `down --purge`.

## Root causes

1. Ownership is asserted from the state file and never read back. D48 checks only that an id string still exists; it does not check the resource behind it is the same one (Herdr reuses ids after a restart), and several paths skip even that check.
2. Apply reports success whenever it recorded a digest, whether or not a backend verb actually made the change (cwd/env/on_start edits, pane reorder, split direction), and never compares what a pane is running to what is declared.
3. Multi-step operations capture a picture once (a plan, an apply state, a snapshot) and trust it across a boundary that can invalidate it (session start, a failing backend call, a pane closing mid-snapshot).

## Grouped fixes

### Group A: plan from the live session (D51)
- Re-snapshot, re-prune and re-plan after `ensure_session` reports it started the session. [partial 2, herdr 1]
- `snapshot()` keeps `process_info: None` for a pane whose process-info call fails instead of failing the whole snapshot; only `session.snapshot` gates reachability. [partial 3, herdr 3]
- `caller_pane_id` is set only when the env id is present in the snapshot's panes. [state 3]
- `lint` opens the backend and prunes like `plan`/`status`. [state 6]
- A corrupt or old-schema state file warns and falls back to empty instead of aborting. [state 5]
- A resolved session name of exactly `default` normalises to no named session so the socket path is the bare one. [herdr 2]
- `stop_failed_because_not_running` finds the JSON object in the stream rather than requiring the whole stream to parse. [herdr 4]

### Group B: apply records progress as it happens (D52)
- `apply_plan_gated` records ownership and saves state per applied action, collects a failing action's error and continues where later actions do not depend on it, and reports failed actions; the retry then reconciles instead of duplicating. [partial 1]
- `status` reports a journal entry with `completed: false` as an interrupted run. [partial 4]

### Group C: never report success for an edit with no backend verb (D53)
- Pane `cwd`/`env`/`on_start` and workspace `cwd`/`env` changes plan as a destructive close-and-recreate (marked `[destructive]`, gated like other destructive actions), never as a label rename. [planner 2]
- Pane reorder or split-direction change plans as `Conflict` with the message "cannot reorder panes in place; remove and re-add the tab", never as `SetRatio` alone. [planner 3]

### Group D: command drift (D54)
- For every `serve` pane, `plan` compares `process_info.command` against the declared argv (after a documented normalisation) and emits `RestartCommand` with reason `drifted: running <observed>` on mismatch; `status` shows `drift`. cwd drift surfaces as the destructive recreate from Group C. [planner 1]

### Group E: identity beyond the id string (D55, D16 read-back)
- State file path includes the resolved backend id and session name alongside the repo root; an old repo-only file is migrated once. [state 4]
- `prune_missing` also drops a workspace or tab whose live label contradicts the recorded label, and a pane whose live cwd contradicts the recorded cwd, so a reused id is not treated as continuity. [state 2]

### Group F: harness
- Fake Herdr can answer any method with a Herdr-shaped error `{code, message}`; tests for A and B use it.
- `tests/herdr_contract.rs` checks the field shapes Drove reads (`root_pane`, `tab`, `tab_id`, `session_stop_failed`) and `resolve_socket_path` against a real `herdr session list` line, not only method names.

## Suggested order
B, A (with F) first: they change what the existing commands do. Then C and D together (planner). E last; its label/cwd checks depend on D's snapshot fields.
