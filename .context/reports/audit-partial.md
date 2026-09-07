# Audit: partial failure, ordering, recovery

Scope: `src/executor.rs` (`up`/`down`/task+hook execution), `src/backend/herdr.rs`
(`ensure_session`, `create_tab`/D49, session stop), `src/cli.rs` (`up_command`,
`down_command`), `src/state.rs`. Read end to end, not grepped.

## 1. A mid-apply backend error discards every resource that apply already
created, so the retry duplicates them (confirmed)

`executor.rs:573-618` (`apply_plan_gated`) loops `plan.actions` and calls
`apply_action(...)?` per action. The `?` means the first backend call that
errors — a socket drop, a Herdr API error, a timeout — returns `Err` straight
out of `apply_plan_gated`, discarding both the `outcomes` vector built so far
and the `ApplyState` (`workspace_ids`/`group_ids`/`pane_ids`) that recorded
every backend id created up to that point.

`up()` (`executor.rs:656-722`) calls this with `?` too
(`executor.rs:690`), so `record_ownership` (`executor.rs:795-991`), the only
place that writes newly-created backend ids into `LocalState`, never runs.
`main.rs:4-9` prints `error: {error:#}` and exits — nothing tells the user
what was actually created.

Concretely: a plan that creates workspace `w1`, then its first tab (D49
root-tab reuse, `apply_herdr::CreateTab`, `executor.rs:1105-1135`), then a
second pane split into that tab. If the split for pane 2 fails inside
`HerdrExt::create_tab` (`herdr.rs:576-579`), the tab and pane 1 already exist
on the backend, but `create_tab` itself returns `Err` and its caller never
sees the partial `TabLayout` — pane 1's id is dropped. The workspace, tab and
pane 1 are now live in Herdr but absent from `state.json`.

What the next `drove up` does: `build_plan` sees none of `w1`, its tab, or
pane 1 as owned (local state has nothing), so it plans `CreateWorkspace` +
`CreateTab` again — a second, now-duplicate workspace/tab/pane sit next to
the orphaned first one. This is exactly the #20/22/24/25/29 shape, just at
apply time instead of read time: state trusted an in-memory picture of what
the backend received that a mid-loop error invalidated, and Drove aborted
instead of recording partial progress.

Smallest fix: make `apply_plan_gated` collect-and-continue (record each
outcome/id as it happens, keep going past a single action's `Err`, surface
failed actions in the returned report) so `record_ownership` still runs
against whatever succeeded; or record ownership incrementally, one
`state.save()` per applied action, the same pattern `down()` already uses
(`executor.rs:378-380`).

## 2. Starting a fresh headless session applies the plan built from the old
session's state, never re-pruned against the new (empty) one (confirmed)

`up_command` (`cli.rs:606-663`) probes reachability once, up front
(`cli.rs:619`), and only prunes local state against a live snapshot when
that probe succeeds (`cli.rs:620-627`). The comment at `cli.rs:609-618`
explicitly says: when the target isn't reachable, "there is nothing live to
prune against; `up` below starts the session and applies against local
state as recorded, unchanged."

But `up()`'s own session start (`executor.rs:671-680`, via
`ensure_session`) can turn an unreachable target into a freshly-started,
empty session in the same call. The `plan` used for that call was already
built (`cli.rs:632`) from the *unpruned* pre-start snapshot — i.e. from
whatever `LocalState` said the *previous* session contained. If that state
says everything is already converged (same digests, same recorded backend
ids from a session that no longer exists — deleted via `drove down`'s
session stop and delete, D47, or just crashed), `plan.actions` is empty,
`was_in_sync` is `true`, and `up()` returns `UpOutcome::AlreadyRunning`
having done nothing at all. The user is told the workspace is already
running; the new session is actually empty.

This is worse than the abort in #1: it's a silent false convergence, not
even an error. It reconciles by luck only when the old and new session
happen to disagree on nothing.

Smallest fix: after `ensure_session` reports `Started` (a fresh
headless start, not `Running`), re-snapshot and re-prune/re-plan before
applying — the same prune this file already does for the reachable-at-probe
case, just also triggered by "we just started it ourselves."

## 3. One pane's `process_info` failure fails the whole snapshot, which
`status`/`plan` then report as "not running" (confirmed)

`HerdrClient::snapshot` (`herdr.rs:112-125`) fetches `session.snapshot`,
then loops every pane and calls `self.pane_process_info(&pane.pane_id)?`
(`herdr.rs:120-122`) — a *second*, separate socket round trip per pane, with
`?`. A transient failure on any single one of those calls (a pane closing
mid-snapshot, a socket hiccup) fails `snapshot()` entirely, even though the
first call (`session.snapshot` itself) already succeeded and the session is
plainly up.

`cli.rs:306-329` (the `status`/`plan` path) treats any `snapshot()` error as
"not running" and prints `not running: cannot reach {backend_id} at ...`
(`cli.rs:322-325`) — a false, misleading message when the real cause is one
pane's process-info query, not a dead session. In `up_command`
(`cli.rs:619-627`), the same failure silently skips the D48 prune (only
`if let Ok(live_snapshot) = &live_snapshot` prunes), so `up` proceeds
against unpruned, possibly-stale state — the same false-convergence risk as
#2, triggered by a one-pane hiccup instead of a session restart.

Smallest fix: collect-and-continue inside `snapshot()` — a pane whose
`process_info` call fails should keep `process_info: None` for that pane
rather than failing the whole snapshot; `session.snapshot` itself (the part
that actually answers "is the session up") should be the only thing gating
reachability.

## 4. The apply journal is written but never read back (confirmed)

`LocalState::begin_action`/`finish_action` (`state.rs:91-113`) exist
specifically to record "an approval-gated `run` started, and whether it
finished" — `begin_action` saves *before* the argv runs
(`executor.rs:110-111`, `executor.rs:175-179`), so a process killed mid-`run`
leaves a `completed: false` journal entry on disk. That's the right save
granularity. But nothing in the codebase ever reads `state.journal` back —
confirmed by grep: every other hit is the doc comment at `executor.rs:117`
or the unrelated Radiator token journal. No CLI command surfaces an
incomplete entry, and `run_task`/`run_hook` don't check for one before
starting a new action with the same digest.

Concretely: `drove run scaffold` is killed while `scaffold`'s `run` argv is
executing. The journal now holds `{action: "task:scaffold", digest: ...,
completed: false}` forever (until it ages out after 100 entries,
`state.rs:115-120`). The task's `ManagedResource` was never written either
(`record_task_resource` runs after `finish_action`, `executor.rs:134-141`),
so the next `drove up`/`run` just reruns the task from scratch — which is
the right recovery for the *task*, but the user has no way to learn "the
previous run may have partially executed `scaffold`'s `run` argv" from
`drove` itself; they'd have to notice the process died.

Smallest fix: on `LocalState::load`, or in `drove status`, report any
journal entry with `completed: false` as "an earlier `{action}` did not
finish (interrupted?) — rerun to retry" — turning the already-correct
save-before-run into something the user actually sees.

## 5. `down --purge` aborts mid-teardown on a `close_pane` failure, and a
retry re-runs that resource's already-executed `on_stop` hook (plausible)

`down()`'s loop (`executor.rs:349-381`) runs a resource's `on_stop` hook,
then (only under `--purge`) calls `backend.close_pane(&resource.backend_id)?`
(`executor.rs:375`), and only afterward removes the resource from state and
saves (`executor.rs:378-379`). If `close_pane` fails, the `?` exits `down()`
immediately — resources torn down in earlier loop iterations are already
saved-removed (fine, idempotent), but *this* resource's hook already ran
(approval consumed is harmless — approvals are digest-keyed and don't
expire — but the hook's own side effect, e.g. killing a process or sending a
notification, already happened) and is not recorded anywhere as "hook already
ran for this resource." A retried `drove down --purge` reruns `on_stop` for
that same resource before attempting `close_pane` again.

This is lower-severity than 1-3 because most `on_stop` hooks are themselves
idempotent by convention (stop-a-process style), and the report file
(`DownReport.hooks_run`) at least reflects only what really ran in that
invocation. Flagging because the brief asks about exactly this shape and the
fix is cheap: run the hook and remove-from-state+close_pane as one step (or
mark the hook run in state before attempting `close_pane`) so a retry skips
a hook it already fired.

Smallest fix: record "hook ran" for a resource (even just removing the
`on_stop` hook, not the resource, from `collect_hooks`'s effective set on
retry) before the `close_pane` call that can fail.

## Systemic cause

Every one of these traces back to the same shape: a piece of local state
(an `ApplyState`, a pre-fetched `Plan`, a `LocalState.journal` entry) is
built or captured once, then trusted for the rest of an operation even
across a boundary — a backend call that can fail (#1, #5), a session
restart that changes the world out from under an already-built plan (#2), or
a transient RPC failure that gets conflated with "the session is down" (#3)
— that invalidates it. The fix pattern is consistent: either don't trust a
captured snapshot across such a boundary (re-probe/re-prune after
`ensure_session` starts something, as #2 needs), or make the by-piece
progress durable as it happens instead of batching a whole plan/loop behind
one `?` (as #1 and #5 need), or actually surface state that's already being
recorded (#4).

## Already fine

- `down()`'s per-resource loop (excluding the `--purge` close_pane case in
  #5) removes and `state.save()`s one resource at a time
  (`executor.rs:378-379`), so an error on resource *k* leaves 1..k-1
  correctly detached and idempotent to retry.
- `record_task_resource` / task journal save-before-run, save-after-run
  (`executor.rs:110-113`) is the right granularity; D18's refusal to record
  a failed `run`'s digest as observed (`executor.rs:131-141`) correctly
  keeps a failed task unconverged so the next `build_plan` retries it.
- `LocalState::save()` writes to a `.tmp` file and renames over the real
  path (`state.rs:49-61`) — a crash mid-write can't corrupt the previous
  good state.
- `run_stop_session` (D47, `herdr.rs:706-737`) correctly treats only the
  documented `session_stop_failed` code as "already stopped, proceed to
  delete," and never lets an undocumented stop failure reach delete
  (`herdr.rs:711-720`); `down_command`'s doc comment (`cli.rs:520-526`) is
  accurate — resources are detached and saved before the session stop is
  even attempted, so a `stop_session` failure still leaves a retry
  idempotent.
- `ensure_session`'s headless-start path (`herdr.rs:634-647`) treats "binary
  missing" and "socket never comes up" both as `CannotStart` with the exact
  hint command, rather than a bare error — good message discipline.
- Socket reads are bounded on every platform via the helper-thread +
  channel pattern in `read_line_with_timeout` (`herdr.rs:951-979`), so a
  hung Herdr server can't hang `drove` forever on `output()`'s readiness
  probe.
