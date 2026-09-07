# Audit: recorded state trusted without checking the live session

Read paths in full: `src/state.rs`, `src/executor.rs`, `src/planner.rs` (build_plan
and every `plan_*` helper), the relevant slices of `src/cli.rs` (`plan`/`status`
routing, `run_command`, `lint_command`, `down_command`, `up_command`), and
`src/backend/herdr.rs` (`snapshot`, `caller_pane_id_from_env`, `close_pane`,
`stop_session`). Skimmed `src/backend/radiator.rs` to confirm it shares the same
`close_pane`/`down` call shape rather than having its own id logic.

## Findings, most severe first

### 1. `drove down --purge` closes panes by recorded id with no live check at all
**File:** `src/cli.rs:527-544` (`down_command`) and `src/executor.rs:335-383` (`down`).
**Trigger:** run `drove down --purge` after the Herdr session was stopped and
restarted (or the target pane was closed and Herdr's id counter reused the same
id for an unrelated new pane) since the last `drove up`.
**What's wrong:** `down_command` never calls `client.snapshot()` and never
prunes. `down()` iterates `state.profile(ctx.profile)` as-is and, for every
`kind == "pane"` resource, calls `backend.close_pane(&resource.backend_id)`
directly (`src/executor.rs:371-376`). Every other read path in this codebase
(`plan`, `status`, `up`) prunes against a live snapshot first (D48); `down` is
the one path that skips it, and it's the one path that runs a destructive verb
against the id. If the id has been reused, `--purge` closes whatever live pane
now holds that id — not the one Drove thinks it owns.
**Smallest fix:** before the teardown loop, fetch `client.snapshot()` and run
it through `state::prune_missing` the same way `up_command`/`plan`/`status`
already do; skip `close_pane` (and the on_stop hook, which also has nothing
live to act on) for anything `prune_missing` drops, but still remove it from
state so `down` stays idempotent. `prune_missing` already returns exactly the
set needed; this is a ten-line change reusing it, not new logic.
**Confidence:** confirmed — read `down_command` and `down()` end to end; no
snapshot call exists anywhere on this path.

### 2. Backend-id reuse defeats `prune_missing` itself, so a stale resource is treated as converged and reconciled in place
**File:** `src/state.rs:176-246` (`prune_missing`), consumed by
`src/planner.rs` (`plan_workspace:376-451`, `plan_group:457-517`,
`plan_normal_pane:598-695`) via `effective_owner`.
**Trigger:** the Herdr session (or a *different* session sharing the same
repo — see #4) hands out `w1`, `w1:t1`, `w1:p1` again after the tracked ones
were closed and the counter reset — plausible after any full session restart,
since ids are assigned by Herdr's own counter, not chosen by Drove.
**What's wrong:** `prune_missing` (and the whole ownership model, per the
`src/planner.rs` module doc: "Real backends read ownership tokens back (D16);
until that lands... build this from `LocalState`") only checks whether the
*id string* is present in the live snapshot, never whether the live resource
at that id is the same resource. A reused id passes the prune untouched, so
`effective_owner` reports it "owned by this profile" and `build_plan` compares
digests: if they happen to differ, the planner emits `RenameWorkspace`,
`RenameTab`, `RestartCommand`, or — worst case, when a pane's recorded parent
no longer matches — `ClosePane` followed by `SplitPane`
(`src/planner.rs:666-691`), all addressed at `observed.backend_id`. That's a
rename or a close-and-recreate applied to a live resource Drove has never
actually seen before, because it happens to have inherited an old id.
**Smallest fix:** none of this is fixable by refining `prune_missing`'s id-set
check alone — it needs a second signal beyond the id string (a label/cwd
sanity check against what was recorded, or the ownership token read-back D16
already anticipates) before treating a matched id as *the same* resource
rather than *a* resource with that id. Until D16 lands, the cheapest
mitigation is to have `prune_missing` also drop (rather than keep) a resource
whose recorded `digest`-adjacent metadata (e.g. workspace label for
workspaces, since that's already carried in `SessionSnapshot`) contradicts
what's live, rather than trusting id equality alone.
**Confidence:** confirmed by reading `prune_missing`, its own doc comment, and
every planner branch that consumes `effective_owner`'s result — this is a real
gap, not a hypothetical, and it's the systemic root of #1, #3 and #4.

### 3. `adopt = "caller"` trusts `$HERDR_PANE_ID` with no check it's a live pane
**File:** `src/backend/herdr.rs:112-125` (`snapshot`, sets
`snapshot.caller_pane_id = caller_pane_id_from_env()` unconditionally) and
`src/backend/herdr.rs:885-887` (`caller_pane_id_from_env`), consumed by
`src/planner.rs:543-559` (`plan_pane`'s `AdoptPane` branch) and
`src/executor.rs:826-844` (`record_ownership`'s `AdoptPane` branch, which
writes `backend_id: caller_id` into managed state with no existence check
either).
**Trigger:** the invoking shell's `HERDR_PANE_ID` is stale — the pane was
closed and Herdr reused the id for an unrelated pane, or the env var was
inherited into a subshell/tmux pane that has since moved — and the profile
declares a pane with `adopt = "caller"`.
**What's wrong:** `caller_pane_id` is read straight from the environment and
never cross-checked against `snapshot.panes` (the very same snapshot it's
attached to). `plan_pane` only checks "is there no existing owner for this
identity" before adopting; it never checks the caller id is present in
`snapshot.resources`/pane ids. `record_ownership` then writes that id into
local state as an owned pane with `adopted: Some(true)`. From that point on,
every future `up`/`down` manages a pane Drove never actually created and may
not even be the one the user is sitting in.
**Smallest fix:** in `HerdrClient::snapshot`, only set `caller_pane_id` when
`caller_pane_id_from_env()`'s value is present in the snapshot's own
`panes` list; otherwise leave it `None` so adoption falls through to a normal
create/skip instead of adopting a phantom.
**Confidence:** confirmed — read `snapshot()`, `caller_pane_id_from_env`, and
both consumers; there is no liveness check anywhere on this path.

### 4. Local state is keyed only by `repo_root`, not by the resolved session/target — switching sessions replays another session's ids
**File:** `src/state.rs:276-279` (`state_path`, hashes only `repo_root`) and
`src/cli.rs:881` area (`resolve_backend`, which picks `backend_id`/`target`
per-invocation from flags/config, independent of what's on disk).
**Trigger:** the same repo is used against two different Herdr sessions (e.g.
`drove --session foo up` then later `drove --session bar up`, or a profile
whose backend/target changes between runs) — one `~/.local/state/drove/…json`
file is shared across both.
**What's wrong:** `LocalState::load`/`state_path` never take the session name,
backend id, or profile's resolved target into account — only the repo path.
Switching targets doesn't clear or namespace the record, so `prune_missing`
compares session B's ids against a profile that was actually built against
session A. Because Herdr ids are small and workspace-scoped (`w1`, `w1:t1`),
it's entirely plausible for session B's own first workspace to also be `w1`
— which, per finding #2, sails straight through the prune and gets planned as
already-owned, converged or in need of a rename that lands on session B's
own unrelated workspace.
**Smallest fix:** fold the resolved backend id and target/session name into
`state_path`'s hash alongside `repo_root`, so each (repo, backend, session)
triple gets its own state file; a first run against a new target then
correctly sees nothing recorded and creates fresh, rather than colliding with
another target's record.
**Confidence:** confirmed by reading `state_path` and confirming no other
call site folds session/target into the state file path or into
`ManagedProfile` lookup.

### 5. A corrupt or older-schema state file aborts the whole command instead of falling back to empty
**File:** `src/state.rs:27-47` (`LocalState::load`).
**Trigger:** `schema_version` (or any other non-`#[serde(default)]` field) is
missing or the wrong shape — a partially-written file from a crash mid-`save`
before the D-something that added a field, or hand-editing during debugging.
**What's wrong:** `load` only special-cases `ErrorKind::NotFound`; any
`serde_json::from_slice` failure (missing required field, wrong type)
propagates as `Err(...).context("invalid Drove local state")` all the way up
through `plan`/`status`/`up`/`down`/`run`/`lint`, so the command that could
otherwise reconcile safely against the live backend (everything downstream
already prunes stale ids in most paths — see #1 for the exception) instead
refuses to run at all until the file is manually deleted.
**Smallest fix:** on a deserialize error, log a warning and fall back to the
same empty-state branch as `NotFound` — reconciling from a blank slate is
exactly what `prune_missing`/`build_plan` are designed to do safely from a
missing state, so a corrupt one should be treated the same rather than
special.
**Confidence:** confirmed by reading `load`; no schema-migration or
degrade-to-empty path exists.

### 6. `drove lint` and `drove run` never touch the live session, so their local-only view can silently disagree with reality
**File:** `src/cli.rs:419-457` (`run_command`), `src/cli.rs:461-518`
(`lint_command`).
**Trigger:** the session was restarted/wiped since the last `up`/`plan`, and
the user runs `drove lint` (checks a `was =` against `is_live`, which is
`managed.to_snapshot(...)` built from local state alone, never pruned) or
`drove run <task>` (whose `on_start`/`on_stop` hooks get `DROVE_BACKEND_ID`
from the raw recorded `resource.backend_id`, unpruned, via
`src/executor.rs:353-358` in `down` and similarly in `run_task`'s hook call).
**What's wrong:** neither command opens a backend client or snapshots it at
all, so `lint`'s "matches nothing live" check and any hook's
`DROVE_BACKEND_ID` are answered purely from a local record that may be stale
in either direction (says "live" when it's gone, or vice versa). This is
lower severity than #1-#4 because `lint` is advisory-only (never applies
anything) and a task's own `run` argv runs on the host, not against the
backend — the backend id only reaches an informational env var. Still fits
the brief's pattern: an assumption from local state, presented as fact,
without checking whether it's true right now.
**Smallest fix for `lint`:** open the backend client and prune before
building the snapshot passed to `is_live`, same one-liner as `plan`/`status`.
Leaving `run`'s hook env var unpruned is probably fine to leave as-is (it's
informational only), but worth a one-line doc note if not fixed.
**Confidence:** confirmed by reading both commands; plausible-but-low-severity
for the hook env var half.

## Systemic cause

Ownership in this codebase is not read back from the backend (D16 hasn't
landed) — it is an assertion, replayed unpruned from `LocalState`, and
`prune_missing` (D48) plugs the *most obvious* leak (an id that no longer
exists at all) but not the deeper one: a live backend id is treated as proof
of continuity, when Herdr's own id counters make id *reuse* — after a
restart, or across two sessions sharing one repo's state file — entirely
possible. Every path that skips even the shallow prune (`down --purge`,
`lint`) inherits the same risk one level worse, since it doesn't even check
existence.

## Already fine

- `up`/`plan`/`status` (`src/cli.rs:291-361`, `606-728`) all fetch a live
  snapshot and run `prune_missing` before building the plan; `up_command`
  additionally persists the pruned set back to state before applying, so a
  dropped resource isn't re-dropped on every subsequent run.
- `prune_missing`'s workspace→placement→pane cascade is correct and covered
  by its own tests (`src/state.rs:397-452`): a missing workspace drops its
  placements and panes via the `w1:` id-prefix rule, a missing placement
  drops its panes by recorded `parent` identity (not by id prefix, since a
  placement's backend id — a tab id — isn't a prefix of its panes' ids).
- A user renaming a workspace/tab/pane's *label* directly in Herdr (bypassing
  Drove) is handled correctly by design, not a bug: the id stays live, so
  `prune_missing` keeps it, and `build_plan` sees the digest mismatch and
  proposes `RenameWorkspace`/`RenameTab`/`RenamePane` back to the declared
  label — exactly the "content update" path the ownership model is meant to
  drive.
- The D34 `was =` rename logic (`plan_workspace`/`plan_normal_pane`) correctly
  refuses to guess when both the old and new identity are simultaneously live
  and owned (`identity_conflict`), rather than picking one silently.
- `record_ownership` and `ApplyState::seeded_from` (`src/executor.rs:531-556`,
  `795-991`) only ever run downstream of `up`'s own prune (finding #2's caveat
  aside), so within a single `up` invocation they're internally consistent.
