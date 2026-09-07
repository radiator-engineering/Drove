# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `drove up` now records ownership and saves state after each action
  succeeds, rather than only after the whole plan finishes, so a failure
  partway through no longer loses track of what already landed. A failed
  action no longer aborts the plan: independent actions still apply, actions
  that depend on the failed one are skipped and reported as
  `skipped: ID: depends on PARENT`, and `up` exits 1 after printing
  `failed: ID: ERROR`/`skipped: ...` lines for everything affected. The next
  `up` plans only what's still missing. `drove status` now reports a journal
  entry an apply began but never finished as `interrupted ACTION (DIGEST)`,
  and `drove run` warns `previous run of NAME did not finish; rerunning`
  before rerunning that task.
- `drove down --purge` no longer aborts when the session has already lost a
  resource it recorded: the pane, tab, or workspace is pruned from local
  state instead of aborting on `pane_not_found`, and a `close_pane` failure
  for a resource that's still there no longer stops the teardown either
  (#29).

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
