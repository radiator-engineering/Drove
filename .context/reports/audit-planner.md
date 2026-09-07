# Audit: editing a running layout in place, and drift detection

Scope: Starlark loader/model, `src/ir.rs`, `src/planner.rs`, `src/executor.rs`,
`src/state.rs`, `tests/cli.rs`. Read-only; commit `ca1a3e2`.

## (A) Edit cases: what happens today

All rows assume the edited resource is otherwise converged and owned by the
current profile (recorded `backend_id` still lives in the live snapshot, so
D48's prune does not intervene first).

| Edit | What happens today | Correct? | Evidence |
|---|---|---|---|
| Pane's `serve` argv changes | `RestartCommand`: `backend.restart_command(id, argv)` | Right | `planner.rs:697-716` (`push_pane_content_change`), `executor.rs:1074-1079`; test `changed_serve_command_restarts_in_place` |
| Pane's `cwd` changes (non-serve pane) | Digest changes → `RenamePane`, but `apply_core::RenamePane` only calls `backend.rename_pane(id, label)` — cwd is never re-applied to the live pane | **Unsafe/silent**: state is marked converged (new digest recorded) even though the pane's actual cwd never changed | `ir.rs:144-151` (cwd in pane digest), `planner.rs:717-730`, `executor.rs:1067-1073`. No CLI test asserts the pane's cwd actually changes on the backend after this edit — `tests/cli.rs` has no case for it |
| Pane's `env` changes | Same as `cwd`: digest changes → `RenamePane` (label-only) | **Unsafe/silent**, same as above | `ir.rs:144-151`, `executor.rs:1067-1073` |
| Add a pane to an existing tab | `SplitPane` into the group | Right | `planner.rs:641-655`; test `missing_pane_in_existing_group_splits_it` |
| Remove a pane (still declared? no — no longer declared) | `Detach`: local state stops tracking it, nothing closed | Right, by design (module doc: only `drove down` closes) | `planner.rs:1-22`, `943-978`; test `owned_but_undeclared_pane_is_detached_not_closed` |
| Reorder panes within a tab | Included in `topology_digest` → digest mismatch → `RenameTab` + `SetRatio` only. **No backend verb exists to move an existing pane's physical position** (`HerdrExt` has no reorder/move verb) | **Unsafe/silent**: `drove up` reports success and records the new digest, but the panes are never physically reordered; `SetRatio` then applies the new ratio list positionally to the *old* physical order, which can put the wrong ratio on the wrong pane | `ir.rs:94-99,181-193` (order is part of `topology_digest`), `planner.rs:485-517`, `backend/mod.rs:152-212` (no move/reorder verb) |
| Change split ratios only | Same path (`RenameTab` + `SetRatio`); `SetRatio` is a real verb here, so this one is correctly applied | Right | `planner.rs:502-513`; `HerdrExt::set_ratio` |
| Change a tab's split direction (row↔column) | Also folded into `topology_digest` → same `RenameTab`+`SetRatio` path; **no verb re-splits an existing tab** | **Unsafe/silent**, same root cause as reorder | `ir.rs:94-99`; `backend/mod.rs:152-212` (no re-split verb) |
| Rename a tab (label only) | `RenameTab` | Right | `planner.rs:490-501` |
| Rename a workspace (label only) | `RenameWorkspace` | Right | `planner.rs:433-449` |
| Change workspace `cwd`/`env` | Folds into workspace digest → `RenameWorkspace`, but `apply_core::RenameWorkspace` only calls `backend.rename_workspace(id, label)` — cwd/env never re-applied | **Unsafe/silent**, same shape as pane cwd/env | `ir.rs:198-201`, `executor.rs:1049-1055` |
| Add/remove a tab (whole placement group) | Add: `CreateTab`. Remove: `Detach` on the group id, and independently on each pane under it (not cascaded like D48's `prune_missing` cascade — each resource is checked against `declared` individually, which still works because pane ids are also no longer declared) | Right | `planner.rs:453-517, 943-978` |
| Add/remove a workspace | Add: `CreateWorkspace` + `CreateTab`. Remove: `Detach` on workspace/tab/pane individually, same as above | Right | `planner.rs:376-452` |
| Change `caller_pane`'s target pane (move `adopt = "caller"` to a different pane) | The old pane keeps its previous digest fields (no more `adopt`) → falls to `plan_normal_pane`, content-changed → `RenamePane` only, not re-verified against what's actually running there. The new pane goes through `AdoptPane`. Net effect: the label-only-rename gap above also applies to a pane that stops being the caller adoptee. | **Unsafe/silent**, same root cause | `planner.rs:528-596` |
| Change a task's argv (approval digest) | `RunTask`, gated by the existing digest-approval flow (D21) — this path is correctly wired | Right | `planner.rs:885-940` |
| Change the session name | Not a resource edit; `drove` resolves target by CLI/config, not by declared state. Out of scope for drift — no planner action exists or is expected here | N/A | `cli.rs` target resolution (not diffed here in depth) |
| Change `on_start` | Folds into pane/task digest → for a pane, same `RenamePane`-only silent gap as cwd/env (the hook only ever fires once, at creation, per `executor.rs:78`); for a task, correctly triggers `RunTask` | **Unsafe/silent for panes**; right for tasks | `ir.rs:144-151`, `executor.rs:78-120` |

## (B) Ranked findings

1. **No comparison of a pane's live command/cwd to its declared state — `drove status`/`plan`/`up` are blind to manual drift.**
   `src/planner.rs` (whole diff), `src/state.rs:139-159` (`ManagedProfile::to_snapshot`), `src/backend/mod.rs:43-49` (`ProcessInfo { command, pid }`).
   Trigger: a user runs a different command in a managed pane by hand, or the pane's process dies and gets replaced by a shell — anything that changes what's *actually running* without going through Drove.
   Wrong behaviour: the plan is built only from the profile's declared digest vs. the digest *recorded in local state at the last successful apply* (`ManagedProfile::to_snapshot`). It never asks the backend what the pane is currently running. `Backend::process_info()` fetches exactly `{command, pid}` and its own doc comment says it exists for "drift detection when a digest-based ownership token is unavailable" — but nothing outside test doubles ever calls it (`grep` shows call sites only in `executor.rs` test fixtures). `drove status` will report "in sync" even though the pane is running something else entirely.
   Smallest fix: in `plan_normal_pane`/`push_pane_content_change`, when a backend advertises `capabilities().process_info`, fetch `process_info(backend_id)` for `serve` panes and compare `.command` against the declared argv before trusting the digest; mismatch → `RestartCommand`, independent of digest.
   Confidence: high (structural — confirmed by reading the full planner and the `Backend` trait; this is exactly the shape of bugs #20/22/24/25/29 the user described, generalized from "trusted a recorded id" to "trusted a recorded digest").

2. **Editing a pane's `cwd`, `env`, or `on_start` (non-`serve` panes) is silently a no-op on the live pane, but `drove up` reports success and updates state.**
   `src/ir.rs:144-151` (all three are in the pane content digest), `src/planner.rs:717-730` (`push_pane_content_change` routes non-serve changes to `RenamePane`), `src/executor.rs:1067-1073` (`RenamePane` calls only `backend.rename_pane(id, label)`).
   Trigger: change `cwd =` (or `env =`, or `on_start =`) on a pane with no `serve` command, then `drove up`.
   Wrong behaviour: the plan computes a content digest mismatch, emits `RenamePane`, apply only renames the label, and the pane's recorded digest becomes the new one — Drove now believes the pane matches its declaration even though the live pane's working directory / env / one-shot hook never changed. This is the literal bug behind "I am finding it hard to update current layouts in place": there is currently no path that makes a `cwd` edit take effect on a running pane at all.
   Same bug, same root cause, at the workspace level: `RenameWorkspace` (`executor.rs:1049-1055`) only calls `backend.rename_workspace(id, label)`, so a workspace `cwd`/`env` edit is equally silent.
   Smallest fix: give `RenamePane`/`RenameWorkspace` a companion action (or fold into `RestartCommand`'s reach) that detects "declared cwd/env differ from what was last applied" and either applies them via a `cd`/re-exec in the pane, or explicitly marks the pane `[destructive]` (close+recreate) when there is no in-place way to change a pane's cwd — but do not report success while silently dropping the edit.
   Confidence: high (confirmed by reading `ir.rs` digest construction and `executor.rs`'s exhaustive match; no CLI test exercises a `cwd`/`env`/`on_start` edit against a live/fake backend to catch this).

3. **Reordering panes or changing a tab's split direction changes the topology digest but nothing re-applies the new topology — `SetRatio` can then misapply ratios to the wrong physical pane.**
   `src/ir.rs:94-99,181-193` (`topology_digest` covers `split`, `ratios`, and ordered pane names), `src/planner.rs:485-517` (`plan_group`'s only response to any topology digest change is `RenameTab` + `SetRatio`), `src/backend/mod.rs:152-212` (`HerdrExt` has `split_pane`/`set_ratio`/`rename_tab` — no move/reorder/re-split verb).
   Trigger: swap the declared order of two already-existing panes in the same tab, or flip a tab from row to column split, with no pane added/removed.
   Wrong behaviour: `plan_group` sees the digest differ and only issues `RenameTab`+`SetRatio`; there is no verb that could reorder live panes or re-split a tab even if one were planned. `SetRatio` then applies the new ratio list by position to panes that were never actually moved, so a ratio meant for pane B can land on pane A. The plan reports success and records the new topology digest as converged.
   Smallest fix: at minimum, detect a pane-order or split-direction change that isn't also a placement-group move, and downgrade that response to `Conflict`/an explicit "unsupported: cannot reorder panes in place, close and reopen the tab" rather than a `SetRatio` that silently mismaps ratios.
   Confidence: medium-high (confirmed the digest includes order/split and that no reorder verb exists; have not run this against a live Herdr session to observe the exact ratio-to-pane mismatch, since that requires a live backend).

4. **`drove status`/`plan` reachability check races the actual `up` path — no shared finding beyond documented D48 pruning; noted only as scoped-out.** *(withdrawn — D48's `prune_missing` is correctly wired into both `status`/`plan` (`cli.rs:340-341`) and `up` (`cli.rs:619-627`); this is not a new hole.)*

## (C) Design sketch: detecting and fixing command drift (< 200 words)

Herdr's snapshot already gives Drove `PaneInfo.process_info` (`command: Vec<String>`, `pid`) per pane when `capabilities().process_info` is true, plus `cwd`. It does **not** give a hash of "declared vs. running" — that comparison has to happen in Drove.

At plan time, for any `serve` pane whose recorded digest matches the declared one (so today's diff says "converged"), additionally fetch `process_info` and compare `.command` to the declared argv (allow a documented normalization, e.g. ignoring a wrapping shell `-c`). A mismatch becomes a new, non-content-digest-driven trigger for `RestartCommand`, with its own reason string ("pane is running `<observed>`, not the declared command") so it's visually distinct from "declared file changed."

`cwd` drift (declared cwd vs. `PaneInfo.cwd`) can't be fixed with a restart-in-place verb Drove has today — `RestartCommand` only replaces the foreground command, not the shell's cwd. So a `cwd` mismatch should surface as a `[destructive]` `ClosePane`+`SplitPane` (recreate), the same pattern already used for a placement-group move, since Herdr exposes no verb to `cd` an existing shell in place.

## (D) Already fine

- **Ownership/adoption never matches by label.** Backend labels (`WorkspaceInfo.label`, `TabInfo.label`) are cosmetic, set only by `RenameWorkspace`/`RenameTab`; the planner always keys by the resource's own declared identity through `LocalState`'s recorded `backend_id`, never by label. Two live tabs sharing a label, or a user renaming a tab out from under Drove, cannot cause an adoption mix-up — Drove will just keep calling `rename_tab` with the digest-driven label it expects (potentially fighting a user's manual rename, but never adopting the wrong resource).
- **Stale recorded ids from a wiped/restarted session (D48).** `prune_missing` (`state.rs:176-246`) is correctly invoked before every `plan`/`status`/`up` build (`cli.rs:340-341,619-627`), including cascading a missing workspace's placements and panes and a missing placement's panes. This is the fix pattern the rest of this report's findings need generalized to *content*, not just *existence*.
- **`RunTask`/task approval-digest flow.** Task argv changes correctly re-trigger `RunTask` gated by the existing approval mechanism (D21); ordering via `after` and input-overlap conflict detection (`plan_tasks`) both have direct test coverage and look sound.
- **Unmanaged resources are never touched.** Confirmed by `unmanaged_panes_produce_no_actions` and the `effective_owner` filter used everywhere in the planner — a resource observed but not owned by this profile is always skipped, in every code path checked.
- **Duplicate identity names are already rejected at the declaration level** (`Profile::validate`), so the identity-collision half of the "adoption collision" concern in the brief doesn't reach the planner at all.

AUDIT DONE: .context/reports/audit-planner.md
