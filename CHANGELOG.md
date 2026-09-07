# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- Local state now checks a recorded resource's identity, not just whether
  its backend id string still exists. A profile last saved against a
  different backend/session is treated as empty for this run (`state
  recorded for OLD; starting fresh for NEW`) instead of matching another
  session's ids by coincidence. Within one session, a workspace or tab whose
  live label no longer matches what Drove recorded, or a pane whose live
  `cwd` no longer matches, is pruned and reported as `id reused` (instead of
  `not in session`) and planned as a fresh create — Herdr restarting its id
  counter no longer risks a rename or a close-and-recreate landing on an
  unrelated live resource that happens to share the old id. A workspace or
  tab that is still empty on the live side (its only tab, or only pane, is
  idle and nothing else) is exempt from this: the bare root a just-restarted
  session hands back under the same ids is kept and renamed in place rather
  than pruned as reused, which would otherwise strand it unrenamed while a
  fresh workspace is created alongside it.
- `drove up` now records ownership and saves state after each action
  succeeds, rather than only after the whole plan finishes, so a failure
  partway through no longer loses track of what already landed. A failed
  action no longer aborts the plan: independent actions still apply, actions
  that depend on the failed one are skipped and reported as
  `skipped: ID: depends on PARENT`, and `up` exits 1 after printing
  `failed: ID: ERROR`/`skipped: ...` lines for everything affected. The next
  `up` plans only what's still missing. A task whose `after` names one that
  failed (or was itself skipped) is no longer run either: it's reported the
  same way, and `drove run`'s per-task output gains a matching outcome.
  `drove status` now reports a journal entry an apply began but never
  finished as `interrupted ACTION (DIGEST)`, and `drove run` warns
  `previous run of NAME did not finish; rerunning` before rerunning that
  task. A partial apply's summary line and `--json` `"status"` no longer
  claim `in_sync`; a `state.save()` failure now stops `up` from applying
  further actions it would have no record of.
- The planner never reports success for an edit it can't apply in place: a
  pane's `cwd` or `env` change now closes and re-splits the pane
  (`[destructive]`) instead of a no-op rename, a workspace's `cwd`/`env`
  change cascades the same recreation to every pane that inherits it, and
  reordering panes or changing a tab's split direction is a `Conflict`
  instead of silently misapplying ratios to the wrong pane (#33).
- `plan`/`status`/`up` detect command drift: a `serve` pane whose backend
  reports a different running command than the one declared — or nothing
  running at all — is planned as `RestartCommand`, independent of whether
  the recorded digest still matches (#33).
- `drove down --purge` no longer aborts when the session has already lost a
  resource it recorded: the pane, tab, or workspace is pruned from local
  state instead of aborting on `pane_not_found`, and a `close_pane` failure
  for a resource that's still there no longer stops the teardown either
  (#29).
- `drove up` against a target that wasn't reachable yet no longer applies a
  plan built from local state as recorded before the session was started:
  once `ensure_session` reports it just started the session, `up` re-fetches
  the now-live snapshot, re-prunes local state, and re-plans before applying
  anything or reporting `focused`. The report gains `"session_started"` so
  callers can tell a fresh start from an already-running session.
- `HerdrClient::snapshot`: a failing `pane.process_info` call for one pane no
  longer fails the whole snapshot. That pane's `process_info` is left `None`
  and its id is listed under the new `"process_info_unavailable"` field of
  `status --json`; only the `session.snapshot` call itself decides
  reachability.
- The caller's pane id (`HERDR_PANE_ID`) is only trusted when the live
  snapshot actually lists it. A stale or leaked value — a closed pane whose
  id was reused, or the variable escaping into an unrelated shell — no
  longer makes a pane declaring `adopt = "caller"` silently adopt a phantom
  pane; it plans as a normal create instead.
- `drove lint` now fetches the live backend snapshot and prunes recorded
  state against it, exactly as `plan`/`status` do, before deciding whether a
  `was =` reference is still live. A recorded resource whose backend id the
  session no longer has is no longer reported as live just because local
  state still remembers it. When the backend is unreachable, `lint` says so
  and treats nothing as live.
- `LocalState::load`: a state file that exists but fails to deserialize is
  moved aside to `<path>.json.corrupt-<timestamp>` with a printed warning,
  and loading continues from empty, instead of failing the command outright.
  A file missing `schema_version` still loads.
- A Herdr target name of exactly `default` — from a flag, an env var, or the
  Drovefile — now resolves the same as leaving it unset, to Herdr's bare
  default socket, instead of a per-session socket path that could never
  exist.
- `stop_failed_because_not_running` now scans a stopped session's output
  line by line for the first line that parses as the documented
  `session_stop_failed` error, instead of requiring the whole trimmed output
  to parse as one JSON value; a leading non-JSON warning line no longer
  makes `drove down` treat an already-stopped session as a real failure.

## [0.1.1] - 2026-09-07

### Added

- `drove down` stops and deletes the Herdr session it targets, once the
  detach is saved, when that session is a named one (not `default`).

### Fixed

- Herdr backend: the first declared tab of a workspace Drove creates now
  reuses the root tab Herdr's `workspace.create` always hands back, instead
  of opening a new tab and leaving that root tab stray (#25).
- `up`, `plan`, and `status` now prune local state against the live backend
  snapshot before building a plan: a recorded resource whose backend id no
  longer exists (a Herdr session stopped and restarted, which wipes its
  workspaces and id counter) is dropped and planned as a fresh create
  instead of trusted as already there, which previously left `status`
  reporting `in_sync` and `up` failing with `workspace_not_found`. `status`
  reports each pruned resource under a new `recreate` line, and its `--json`
  report gains a `"pruned"` list of the dropped identities.

## [0.1.0] - 2026-09-07

First release.

Drove compiles a checked-in `Drovefile` to a canonical model, compares it to
the live state of a backend, and reconciles the difference.

### Added

- Core model, IR, and DSL: a `Drovefile` declares workspaces, tabs, panes,
  agents, and one-shot setup tasks; Drove compiles it to a canonical,
  content-digested model and diffs it against live state to plan create,
  change, and task actions.
- Herdr backend: incremental convergence, token-gated approval of new or
  changed tasks, and readiness probes (output match or port).
- Radiator hub backend, reading the hub's real reported capabilities instead
  of a hardcoded set.
- A `Backend` core trait plus flavor extension traits (`herdr`, `radiator`),
  so the planner only emits actions every backend can perform and each
  flavor's own verbs (tabs, splits, ratios for Herdr) live in its own
  namespace rather than a lowest-common-denominator union.
- Project-level and profile-scoped backend and target selection
  (`backend(...)`, `profile(session = ..., backend = ...)`), with resolution
  order flag > explicit env > profile > Drovefile > ambient host env >
  built-in default.
- Rename without loss: a resource keeps its live pane across a `was = "..."`
  rename instead of being recreated; `drove lint` catches a stale rename or
  a task with no `check`.
- `drove`, run with no subcommand, resolves a profile (`default`, or the
  file's only profile), starts the target Herdr session if needed, applies
  the plan, and brings the session's first workspace to the front —
  attaching the caller's terminal to it if not already inside Herdr.
- `drove ls`, `plan`, `status`, `render`, `lint`, `run`, `down`, each taking
  an optional positional profile name (`--profile NAME` is an alias).
- v2 to v3 Drovefile migration shims, with `drove render` printing the v3
  form of a v2 file and its deprecation warnings.
- A safety model: Drove only touches resources recorded in its
  machine-local ownership state, never deletes a live resource on
  detachment, requires `--allow-replace` for a topology change that
  discards scrollback, and gates a new or changed task's `check`/`run` on
  approval.

### Fixed

- Windows CI leg and working-directory digest portability.

[Unreleased]: https://github.com/radiator-engineering/Drove/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/radiator-engineering/Drove/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/radiator-engineering/Drove/releases/tag/v0.1.0
