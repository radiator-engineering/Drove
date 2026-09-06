# Drove — Versioned Terminal Workspaces

**Created:** 2026-09-05
**Status:** Ready for implementation

## Core Value

A repository can define, share, and recreate its terminal working environment with one command without taking control of unrelated user resources. Drove versions the workspace; the backend supplies the terminal. Herdr and the Radiator hub are backends, and Herdr is the first flavor — a backend's own placement and layout vocabulary sits alongside a shared core every backend honors in full.

## Problem Statement

Terminal layouts are currently assembled through imperative scripts that create and connect workspaces, panes, agents, and supporting processes. Those scripts encode one layout well, but the resulting workspace is difficult to inspect, adapt, share, and version as part of a repository. Developers need a repository-owned `Drovefile` that can report drift and safely reconcile its managed resources on demand, on whichever backend the project targets.

## Requirements

### Must Have

- A repository can contain a human-readable, committable `Drovefile`.
- A user can establish the repository's default workspace by running one command from that repository.
- A repository can define a default profile and additional named profiles.
- Running the command compares the selected profile with the current Herdr state before making changes.
- Running the command reconciles the selected profile and then exits.
- Running the command repeatedly against an already-matching workspace makes no changes.
- Drove reports whether a profile is `in sync`, `out of sync`, or `not running` without requiring reconciliation.
- A `Drovefile` can declare workspaces, panes, agents, long-running commands, and health expectations, using the core vocabulary every backend supports plus a backend's own flavor (Herdr's `tab` and split placement, for example).
- A `Drovefile` can declare explicit, repeatable bootstrap tasks for local setup.
- New or changed bootstrap tasks are shown to the user and require approval before execution.
- Approval remains valid until the corresponding bootstrap task changes.
- Resources not owned by the selected profile are preserved during reconciliation.
- Machine-specific runtime identifiers and process state do not create repository changes.
- A teammate can clone the repository and reproduce the same logical workspace even when local Herdr identifiers differ.
- Drove clearly identifies which declared resources are out of sync and what reconciliation changed.

### Should Have

- Definitions can reuse repository-local modules and presets without copying layout declarations.
- The existing log-driven workspace can be represented as a reusable preset.
- Users can preview reconciliation changes without applying them.
- Herdr can display that the current profile is out of sync without automatically repairing it.
- A profile can provide optional host-specific adaptations while keeping the shared logical layout intact.

### Out of Scope

- Continuous reconciliation — drift is repaired only when Drove is run.
- Deleting or rearranging unmanaged panes, tabs, or workspaces.
- Rewriting tracked project files during reconciliation.
- A public module registry.
- Replacing supervision internal to reactors or agents.

## Constraints

- **Definition language:** `Drovefile` uses constrained Starlark.
- **Versionability:** Shared definitions must produce stable, useful code-review diffs.
- **Safety:** Reconciliation must preserve unmanaged resources.
- **Trust:** Changed bootstrap behavior requires renewed approval.
- **Lifecycle:** Drove is an on-demand command, not a persistent controller.
- **Compatibility:** Drove supports macOS, Linux, and Windows.

## Key Decisions

### Core and Flavors

- **Decision:** Drove has a core every backend fully honors — workspace, pane, task, profile — plus per-backend flavors for terminology only that backend understands. Herdr's flavor holds `tab`, split placement, and ratios; a Drovefile using only the core reconciles on any backend with no `unsupported` outcomes.
- **Alternatives considered:** Making Herdr's tabs and splits part of the core resource model; one union trait covering every backend's verbs.
- **Rationale:** Tabs and splits are Herdr's own display concept, not something every backend has. Treating them as core forced other backends to fake or silently drop them. Placement, not the resource itself, is the flavor concern.

### Reconciliation Model

- **Decision:** Reconcile only when invoked and otherwise report drift.
- **Alternatives considered:** Continuous Tilt-style reconciliation; create-once behavior.
- **Rationale:** This avoids another always-running controller while retaining repeatability.

### Ownership Model

- **Decision:** Reconcile only resources owned by the selected profile.
- **Alternatives considered:** Exact-layout enforcement; confirmation before touching unrelated resources.
- **Rationale:** Extra user-created resources are not evidence that the declared workspace is wrong.

### Definition Scope

- **Decision:** Manage runtime layout plus explicit, repeatable bootstrap tasks.
- **Alternatives considered:** Runtime layout only; unrestricted repository mutation.
- **Rationale:** Local hooks and configuration are necessary for complete workspace activation, while tracked files remain ordinary repository content.

### Definition Format

- **Decision:** Use a Starlark `Drovefile`.
- **Alternatives considered:** TOML/YAML manifests; executable scripts.
- **Rationale:** A constrained DSL supports readable declarations and reusable presets while remaining suitable for drift calculation.

### Profiles

- **Decision:** Support a default profile and named alternatives.
- **Alternatives considered:** One definition per repository; unrelated workspace files.
- **Rationale:** Everyday, review, release, and incident workflows can share common declarations without duplication.

### Bootstrap Authorization

- **Decision:** Require approval for new or changed bootstrap tasks and remember it until the task changes.
- **Alternatives considered:** Trust the repository once; prompt on every run.
- **Rationale:** This prevents silent execution changes without making routine reconciliation tedious.

## Reference Points

- Tilt's checked-in `Tiltfile` and single-command project activation.
- `/setup-log-driven-workspace` as the first concrete layout to express through Drove.
- Herdr's existing named workspaces, tabs, panes, and agents.
- Herdr's readable-pane and purpose-based naming conventions.

## Future Considerations

- Shared or remotely published workspace modules.
- Automatic drift notifications in Herdr.
- Continuous reconciliation as an opt-in mode.
- Workspace definitions spanning several repositories or worktrees.
