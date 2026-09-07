# Drove v4: one command, named profiles

Date: 2026-09-06. Decisions D41–D45. D1–D40 stay in force. Supersedes the
quick-start shape in the v3 spec: `drove` is the command, the rest are tools.

## 1. Problem

Getting a project's workspace onto the screen takes several commands and a
running Herdr session, and `drove up` today only runs tasks: the
workspace/pane actions the planner emits are printed, never applied
(`src/cli.rs`, `Command::Up`). Profiles exist but share one file-level
backend and session, and are selected by a flag.

## 2. Decisions

**D41 — profile-scoped target.** `profile(name, session = None,
backend = None, ...)`. A profile may name the Herdr session (or, for
`backend = "radiator"`, the hub) it lives in and the backend it reconciles
onto. Unset fields inherit the file-level `backend(...)`,
`herdr.session(...)`, `radiator.hub(...)`. Resolution order per D32 becomes
flag > env > profile > file > built-in. `extends` copies the parent's
`session`/`backend` unless overridden. Model: `Profile { session:
Option<String>, backend: Option<String> }`. `select::resolve` takes the
profile's values as an extra layer between env and file.

**D42 — the profile is positional.** `drove [PROFILE]` and
`drove <SUBCOMMAND> [PROFILE]`. `--profile NAME` stays as an alias (no
warning). With no profile given: the profile named `default` if declared,
else the file's only profile, else exit 2 listing the profiles. Unknown
name: exit 2 with the list. `drove ls` prints every profile with backend,
target name, and whether the target is reachable (`ping` on the resolved
socket). `--json` gives the same as an array.

**D43 — `up` gets you there.** `drove [PROFILE]` (`up`):
1. Resolve backend and target (D41).
2. If the target is not reachable and the backend is Herdr: start the
   session's server headlessly if the Herdr CLI can (`herdr server` with
   the session name; the PR verifies this against the installed Herdr and
   documents what it found); wait for the socket; else fail with the exact
   `herdr --session NAME` command to run. Radiator: fail with the hub name.
3. Apply the plan, not only its tasks: wire `executor::apply_plan` (already
   implemented, never called from the CLI) into `up`, after tasks, with the
   same `--yes` gate for destructive actions. `Conflict` still exits 2.
4. Focus: bring the profile's first declared workspace (or `--workspace
   NAME`) to the front through `workspace.focus`. When the caller's
   terminal is outside Herdr (`HERDR_ENV` unset) and stdout is a TTY, exec
   `herdr session attach NAME` so the user lands in it. `--no-focus`
   skips both. `--json` implies `--no-focus`.
5. Print one line: `profile dev: N created, M changed, K tasks run, in
   sync` or `profile dev: already running, brought to front`.
`plan`, `status`, `render`, `lint`, `run`, `down` are unchanged except for
taking the positional profile.

**D44 — HerdrExt grows session and focus verbs.** `HerdrExt::focus_workspace(id)`
and `HerdrExt::ensure_session(name) -> Result<SessionState>` (`Running`,
`Started`, `CannotStart { hint }`). `ensure_session` shells out to the
`herdr` binary (`HERDR_BIN_PATH` or `PATH`); Radiator gets nothing (D37).

**D45 — Drove dogfoods itself.** The repository root gets a `Drovefile`
declaring the log-driven layout this repo actually runs (control,
maintenance, files; session `drove`) as profile `default`, plus a
`monitoring` profile in session `drove-mon` with one workspace holding
the eventlog viewer and `git log --oneline`. `drove` in this repo brings
the controller layout up; `drove monitoring` opens the second session.
README quick start becomes: write a Drovefile, run `drove`.

## 3. Tests

- D41: model validation (`session`/`backend` per profile; `extends`
  inherits; override wins); `select::resolve` precedence table gains the
  profile layer (flag > env > profile > file > built-in) for both backends.
- D42: positional and flag forms resolve the same profile; no-profile
  rules (default / only / exit 2 with list); `drove ls` text and JSON on
  the log-driven example with an unreachable target.
- D43: with a fake Herdr, `up` applies `CreateWorkspace`/`CreatePane`
  actions (recorded on the fake), then focuses the first workspace; with
  `--no-focus` no focus call; already-in-sync prints the second summary
  form; unreachable target with a session the fake cannot start returns
  the hint and exit 1.
- D44: `focus_workspace` sends `workspace.focus` with the id; `ensure_session`
  on a running socket returns `Running` without spawning anything.
- D45: `cargo run -- render` on the repo Drovefile has no warnings; `drove
  plan` against the live `drove` session reports in sync or only additive
  drift (recorded in the PR body, not a CI test).

## 4. Work split

- **PR 16 `pr16-profile-targets`** — D41, D42 (incl. `ls`). Sonnet.
  Paths: `src/model.rs`, `src/dsl.rs`, `src/backend/select.rs`, `src/cli.rs`
  (argument parsing, profile resolution, `ls` only).
- **PR 17 `pr17-one-command-up`** — D43, D44. Opus 4.8. Paths:
  `src/backend/mod.rs`, `src/backend/herdr.rs`, `src/backend/radiator.rs`
  (accessor default only), `src/executor.rs`, `src/cli.rs` (the `up` path
  only). Runs in parallel with PR 16; second to merge rebases.
- **PR 18 `pr18-dogfood`** — D45 and docs. Sonnet. After both.

## 5. Amendment: ambient host env ranks below the profile (D46)

Herdr exports `HERDR_SESSION` (and Radiator `RADIATOR_HUB`) into every pane
it hosts. Under D41 that ambient value outranked the profile's declared
session, so `drove monitoring` run from inside session `drove` targeted
`drove`, not `drove-mon`. Ambient host variables say where you are, not
where you want to go.

**D46.** Precedence becomes: flag > explicit env (`DROVE_BACKEND`,
`DROVE_SESSION` mirroring `--session`, `DROVE_TARGET` mirroring `--target`)
> profile > file > ambient host env (`HERDR_SESSION`, `RADIATOR_HUB`, the
ambient Radiator detection) > built-in. The precedence-table test gains
the ambient layer for both backends; a test shows a profile with
`session = "drove-mon"` resolving to `drove-mon` while `HERDR_SESSION=drove`
is set, and `DROVE_SESSION=x` still winning over the profile.

## 6. Amendment: `down` stops and deletes a named session (D47)

`drove up` starts the Herdr session a profile names (D43, D44), but
`drove down` left it running: after the hooks and the detach the user still
had to run `herdr session stop NAME` and `herdr session delete NAME` by
hand. Herdr's socket API has no session verbs; only its CLI has them.

**D47.** After running every `on_stop` hook and detaching every owned
resource, `drove down` on the Herdr backend stops and deletes the session
it targets, when that session is a named one. The name is the resolved
target (D46 precedence: `--session`/`--target` > `DROVE_SESSION` >
`herdr.session(...)` > `HERDR_SESSION` when not `default`). With no named
target, or when the target is `default`, `down` never touches the session:
`default` is the user's persistent session, not one Drove created.

`HerdrExt` gains `stop_session(name) -> Result<SessionStop>`, the mirror of
`ensure_session`: it shells out to the `herdr` binary (`HERDR_BIN_PATH`,
else `herdr` on `PATH`) as `herdr session stop NAME` then
`herdr session delete NAME`. A stop that fails because the session is not
running is not an error; delete still runs. `SessionStop { stopped: bool,
deleted: bool }` is reported. A missing binary or a failed delete is an
error, raised only after the detach has been saved to local state, so a
retry of `down` is idempotent.

Output: one more line, `stopped session NAME` (or `deleted session NAME`
when it was already stopped), and the JSON report gains
`"session": {"name", "stopped", "deleted"}` (absent when no named session).
Run from inside the session being stopped, the caller's own pane dies with
it; that is what stop means, so there is no confirmation. The Radiator
backend is unchanged.

Tests (fake `herdr` script on `HERDR_BIN_PATH` that records its argv):
a named session is stopped then deleted, in that order, after the hooks and
detach; `default` and an unnamed target are never touched and the report
carries no `session`; a session that is already stopped is still deleted;
a missing binary is an error and the resources are still detached.

## 7. Amendment: local state is pruned against the live snapshot (D48)

Drove's local state records the backend id of every resource it created.
`plan`, `status` and `up` built the observed snapshot from that record
alone, so a session that had been stopped and restarted (Herdr wipes its
workspaces and restarts the id counter) still looked `in_sync`, and `up`
then failed with `workspace_not_found` when it added a pane to a workspace
that no longer existed (issue 24). A pane Herdr closed after its process
exited (issue 20) and a stale caller pane (issue 22) are the same defect at
pane level.

**D48.** Before a plan is built, the managed set is pruned against the live
backend snapshot: a managed resource whose recorded backend id is not in
the snapshot is dropped from the managed set, together with everything
placed under it (a missing workspace drops its tabs and panes; a missing
tab drops its panes). The planner then sees those resources as absent and
plans their creation; `status` lists each one under a new `recreate` reason
instead of reporting `in_sync`. The prune is saved to local state only when
`up` applies; `plan` and `status` never write. A backend whose snapshot
cannot be read leaves the state untouched and fails as today.

Implementation: one pure function `prune_missing(managed, &snapshot) ->
(ManagedProfile, Vec<String>)` beside `LocalState` (the second value is the
dropped identities, for `status` output and the JSON report's `"pruned"`
list), called from the one place `cli.rs` turns local state into the
planner's snapshot. Tests: a fake snapshot missing a workspace prunes the
workspace and its tabs and panes and the plan creates them again; a missing
pane prunes only that pane; a full snapshot prunes nothing and the plan is
unchanged; `status` prints `recreate` for a pruned resource; `plan` leaves
the state file byte-identical.

## 8. Amendment: the first tab reuses Herdr's root tab (D49)

Herdr's `workspace.create` always returns a root tab (labelled `1`) holding
one idle shell pane. Drove opened each declared `herdr.tab()` as a new tab,
so every workspace it created kept a stray `1` tab (issue 25).

**D49.** When Drove creates a workspace on the Herdr backend, the first
declared tab of that workspace is applied to the root tab Herdr returned
(`apply_layout` with that `tab_id`, then `rename_tab` to the declared
label) instead of opening a new tab. Later tabs open as today. A workspace
Drove adopted or found already present is untouched: the reuse applies only
to a root tab that Drove's own `create_workspace` call produced in this
apply. The root tab's idle shell pane is replaced by the layout, so nothing
running is lost.

Implementation: `create_workspace` on the Herdr client returns the root tab
id alongside the workspace id (a small struct, or the executor reads it
from the same response); the executor passes it to the first `CreateTab`
for that workspace; `HerdrExt::create_tab` gains an `existing_tab:
Option<&str>` parameter. Tests: a fake Herdr records that the first tab's
`apply_layout` carried the root `tab_id` and a `tab.rename`, the second
tab's did not; a pre-existing workspace never gets the reuse; the
`tests/herdr_contract.rs` fixture for `workspace.create` includes the
root tab.

## 9. Amendment: `down` never aborts on a resource the session already lost (D50)

Issue #29. `executor::down` calls `close_pane` for every recorded pane when `--purge` is set, and the first `pane_not_found` aborts the teardown: nothing is detached, the state file keeps the stale ids, and the D47 session stop never runs. D48 prunes stale ids for `up`, `plan` and `status` only.

**Decision D50.** `down` treats a resource the session no longer has as already torn down.

1. In `down_command`, when the backend is Herdr and a live snapshot can be fetched, run the D48 `prune_missing` on the managed profile before calling `down`. Pruned resources are detached from state without any backend call and reported under `"pruned": [ids]` (same shape as D48's `up` report) and one line `pruned <id> (not in session)` per resource. If the snapshot cannot be fetched (session not running, socket gone), `down` proceeds without a backend, prints `warning: session not reachable; detaching without closing panes`, and D47 still runs its stop/delete.
2. In `executor::down`, a `close_pane` error no longer aborts the loop. The resource is still detached and saved; the error is collected in `DownReport::close_failed: Vec<(id, message)>`, printed as `warning: could not close <id>: <message>`, and reported in JSON as `"close_failed": [{"id","error"}]`. The exit status stays 0: `down`'s contract is that Drove stops tracking the resource, and the D47 session delete removes whatever is left.
3. Ordering stays: hooks, detach/close, save, then D47 session stop.

Tests: unit test in `src/executor.rs` with a fake backend whose `close_pane` fails for one id, asserting every id is detached, state is saved, and the report carries the failure; end-to-end test in `tests/cli.rs` with a state file that records a pane the fake session lacks, asserting `drove down --purge` exits 0, prints the `pruned` line, and the state file is empty afterwards; one test with the session unreachable asserting the warning and the D47 stop still running.

## 10. Amendment: plan from the live session (D51)

Audit findings (`.context/reports/audit-summary.md`, group A). Every path that builds a plan or acts on a backend id must take its picture of the session after the session is known to be the one that will receive the work, and one transient failure must not turn into "not running".

**Decision D51.**

1. `up`: when `ensure_session` reports it started the session (not `Running`), `up_command` re-fetches the snapshot, re-runs `prune_missing`, saves, and re-plans before applying. The report gains `"session_started": true`. A plan built before the start is discarded.
2. `HerdrClient::snapshot`: a failing `pane.process_info` call leaves that pane's `process_info` as `None` and continues; only the `session.snapshot` call decides reachability. A `status --json` report lists such panes under `"process_info_unavailable": [ids]`.
3. `caller_pane_id` is set only when the value of `HERDR_PANE_ID` is one of the snapshot's pane ids; otherwise it is `None` and a pane declaring `adopt = "caller"` plans as a normal create with reason `caller pane not in session`.
4. `lint` opens the backend, fetches the snapshot and prunes exactly as `plan` does before answering `is_live`; when the backend is unreachable it says so and treats nothing as live.
5. `LocalState::load`: a file that exists but does not deserialize is renamed to `<path>.corrupt-<rfc3339>` with a printed warning, and loading continues from empty. A missing `schema_version` is tolerated the same way as today's `#[serde(default)]` fields.
6. Target name `default`: `select::resolve` normalises a resolved name of exactly `default` (from any level) to `None`, so the socket path is Herdr's bare `~/.config/herdr/herdr.sock`. The `default` special case in `resolve_socket_path` stays as belt and braces.
7. `stop_failed_because_not_running` scans the stream line by line for the first line that parses as JSON with a `code`, instead of requiring the whole trimmed stream to parse.
8. Harness: the fake Herdr used by `tests/cli.rs` and the unit tests in `src/backend/herdr.rs` can be scripted to answer any method with `{"error": {"code": "...", "message": "..."}}`; the tests for 1 and 2 use it.

Tests: `up` against an unreachable target with non-empty stale state creates every declared resource and never calls focus on a stale id; snapshot with one failing `pane.process_info` still returns the other panes; caller env id absent from the snapshot leads to a create, not an adopt; `lint` prunes; corrupt state file is renamed and the command proceeds; `--session default`, `DROVE_SESSION=default`, `herdr.session("default")` and ambient `HERDR_SESSION=default` all resolve to the bare socket; stop output with a leading non-JSON line is still recognised.

## 11. Amendment: apply records progress as it happens (D52)

Audit findings, group B. `apply_plan_gated` applies actions with `?`, so the first backend error discards every id created so far; `record_ownership` never runs and the retry duplicates the resources.

**Decision D52.**

1. Ownership is recorded and state saved after each successful action, in the same loop, using the current `record_ownership` logic split per action. A killed process leaves state describing exactly what was created.
2. A failing action does not abort the loop. Its error is collected into `ApplyReport::failed: Vec<{action, error}>`; actions that depend on the failed one (a tab in a workspace whose create failed, a pane in a tab whose create failed, a task `after` a failed task) are skipped and reported as `skipped: depends on <id>`. Independent actions continue.
3. `up` exits 1 when any action failed, after printing one line per failure and per skip, and after saving. The next `up` plans only what is still missing.
4. `status` lists journal entries with `completed: false` as `interrupted <action> (<digest>)`, and `--json` reports them under `"interrupted"`. `run` of the same task prints `previous run of <task> did not finish; rerunning` before executing.

Tests: fake backend failing on the second of three creates leaves the first recorded and the third applied when independent, or skipped when dependent; a rerun plans only the failed one; the exit code is 1 with both lines printed; `status` shows an interrupted journal entry.

## 12. Amendment: the planner never reports success for an edit it cannot apply, and detects command drift (D53, D54)

Audit findings, groups C and D. `cwd`, `env` and `on_start` changes on a pane, and `cwd`/`env` on a workspace, plan as a label rename that changes nothing live and record the new digest as converged. Pane reorder and split-direction changes plan as `SetRatio` alone. Nothing compares what a pane runs to what it declares.

**Decision D53.**

1. Pane `cwd` or `env` change: plans as destructive `ClosePane` + `SplitPane` with reason `cwd changed; a pane cannot change directory in place` (or `env changed; ...`), gated exactly like the D22 placement move. Exception: a pane with a `serve` command, when the Herdr `run_command` verb accepts a working directory, plans as `RestartCommand` with the new cwd instead; the worker verifies the verb's parameters against `herdr api schema --json` and documents the outcome in the PR.
2. Workspace `cwd` or `env` change: plans as `RenameWorkspace` plus, for each pane in the workspace that inherits the changed value, the pane rule above. The reason names the workspace.
3. Pane `on_start` change: no backend action; the new digest is recorded and the plan prints `on_start changed for <pane>; runs on next create`.
4. Pane reorder or split-direction change with the same set of panes: plans as `Conflict` with reason `cannot reorder panes or change the split in place; remove the tab and re-add it`. `SetRatio` is emitted only when the pane order and split direction are unchanged.
5. Any `RenamePane` or `RenameWorkspace` action carries only a label change; the planner asserts this and emits `Conflict` with reason `unsupported change: <fields>` for any other digest difference it cannot map to a verb.

**Decision D54.**

1. For every pane with a `serve` command whose backend reports `process_info`, `plan` compares `process_info.command` to the declared argv. Normalisation: trailing whitespace trimmed, a leading `sh -c` / `bash -c` / `zsh -c` wrapper unwrapped, and comparison on the argv vector. A pane whose `process_info` is `None` (shell idle, or unavailable per D51) counts as drifted when a `serve` command is declared.
2. A mismatch plans `RestartCommand` with reason `drifted: running <observed>` (or `drifted: nothing running`), regardless of the recorded digest. `status` shows the pane as `drift` and `--json` reports `"drifted": [{id, declared, observed}]`.
3. `up` applies it like any `RestartCommand`. No new flag; a user who wants a pane left alone removes its `serve`.

Tests: each edit case in the audit's table (`.context/reports/audit-planner.md` section A) has a planner test asserting the action and reason; e2e `up` after a cwd edit shows `[destructive]` and refuses without approval; drift test with a fake `process_info` returning a different command emits `RestartCommand`; `None` with a declared `serve` emits it too; a matching command emits nothing.

## 13. Amendment: identity beyond the id string (D55)

Audit findings, group E. Herdr reuses ids after a restart, and the state file is keyed by repo path only, so a resource is treated as the same one whenever its id string still exists.

**Decision D55.**

1. `ManagedProfile` gains `target: Option<{backend, session}>`, the resolved backend id and session name (`None` for an unnamed session) at the last save. On load, a profile whose `target` differs from the resolved one is treated as empty for this run with the line `state recorded for <old>; starting fresh for <new>`, and overwritten on the next save. A profile with no `target` is stamped with the current one on first load.
2. `ManagedResource` gains `label: Option<String>` and `cwd: Option<String>`, recorded at apply time from what Drove sent. `prune_missing` also drops a workspace or tab whose live label differs from the recorded one and whose recorded label is the one Drove set (a user rename is distinguished by the digest still matching; the worker documents the rule), and a pane whose live `cwd` differs from the recorded one. Dropped ids are reported with reason `id reused` instead of `not in session`.
3. Harness: `tests/herdr_contract.rs` checks the fields Drove reads (`root_pane.tab_id`, `tab.tab_id`, `snapshot.panes[].cwd`, the `session_stop_failed` code) against `herdr api schema --json`, and checks `resolve_socket_path` against the socket column of `herdr session list` for the running default session when Herdr is installed.

Tests: same-id different-label workspace is pruned as reused; same-id same-label is kept; a state file stamped for session `a` used with session `b` plans everything as creates and prints the line; a legacy file is stamped without changing its resources.
