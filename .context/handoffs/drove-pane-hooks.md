# Repair pane startup lifecycle hooks

You are a spawned worker for the Drove controller. Work only in
/Users/jjmartin/Development/Drove/.worktrees/drove-pane-hooks on branch
fix/consumer-pane-hooks. Do not append to the log. Do not git commit, push,
open PRs, or operate live reactors/Herdr layouts. Report to controller in
this pane and write your findings in your final response.

Claim: src/executor.rs and tests/pane_hooks.rs (if integration tests needed).
Pinned baseline: 1b71bf0. Shared contract: consumer-cutover decision 524 and
this brief. Main has unrelated consumer wiring edits; do not touch main.

Confirmed bug: executor::up applies backend pane create/restart actions but
never invokes profile pane on_start hooks (run_hook only used by tasks and
stop traversal). eventlog generated Drove helper depends on on_start running
`eventlog lifecycle start` to register identity/claims before the reactor acts.

Implement lifecycle startup hooks in the actual up path for newly-created
or restarted panes, before their serve command can act. Preserve controller
adoption (no start/restart on adopted caller), approval/journal semantics,
progressive ownership, and dependency failure gating. Hooks must be checked
and run only when startup is needed, not each converged up. A blocked/failed
hook must prevent its pane command and dependent starts, preserve truthful
state, and remain retryable. Avoid duplicate hooks in one apply. Test real
up path using fake backend/runner, including ordering (hook before command),
repeat no-op, failure/blocked handling, and adopted caller preservation.

Keep changes minimal and within scope. If correct solution needs files outside
claim, report a concrete seam before editing them. No new local eventlog
framework; this is a Drove executor correctness fix. Run focused tests and
strict Clippy; report exact changed files, tests and integration guidance.
