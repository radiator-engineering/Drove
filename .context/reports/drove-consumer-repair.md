# Drove consumer repair — 2026-09-07

Status: implementation and disposable verification passed; audited production
recovery is being activated. Historical checkpoints will be preserved below.

Upstream installed baseline: e685252 (eventlog 0.1.0 with setup/action/lifecycle).
Drove baseline: 1b71bf0 (v0.1.2). All 16 cumulative recovery commit refs in
`eventlog-cutover.md` were reverified reachable from current HEAD.

## Ownership and current seams

The Drove controller exclusively owns Drove changes and log writes. The
event-log coordinator (session event-log, w6:p6) owns the reusable commit
command extension. Model labels alone were insufficient: configured command
support is now installed from accepted upstream. Drove's thin Python binding supplies
Composer 2.5 Fast / Claude Sonnet prompts; eventlog retains runtime, scope,
index and outcome ownership.

Drove's pane startup hooks were never called from executor::up. Isolated
worker drove-pane-hooks (gpt-6-astra/high, drove w7K:p1, branch
fix/consumer-pane-hooks, .worktrees/drove-pane-hooks) owns src/executor.rs.
Worker patch accepted after controller review and combined verification;
worker retired and its workspace closed (events 528/529).
No worker commit is authorized; controller integrates the reviewed patch.

## Preserved state

Snapshot directory: `/var/folders/x9/rz97sz4s75z45ym82kdfz87r0000gn/T/drove-consumer-nit8dild`.
It contains all inherited dirty bytes, a Git clone with full history, the
copied log, the Herdr snapshot, and a candidate Drove state tested under an
isolated DROVE_STATE_HOME. The history and inherited source files remain
intact; later changes to DECISIONS.md append the new cutover decision.

The verified layout map is control w1 (controller w1:p1, log w1:p2, monitor
w1:p3); maintenance w2 (lazygit w2:p1, committer w2:p2, docs w2:p3); files
w3 (w3:p1). Each placement's ordered panes was compared with the live
snapshot. No Drove state existed for this repository path. The candidate
one-time ownership import produces only existing-pane command restarts,
with no creates, closes or controller action. No production import yet.

## Checks completed

- Native setup preview/apply/upgrade preserves the log and host Drovefile;
  repeat setup reports no changes.
- Drove and updated log-driven example compile; 48 CLI/example tests pass.
- Historical v2/v3 digest equivalence remains covered by frozen fixtures.
- Disposable lifecycle start registers cursor-committer and doc-worker claims.
- A disposable native docs reaction to the recovered 16-ref acknowledgment
  delivered every ref to Sonnet's prompt and appended exactly
  `paths=docs/recovery-probe.md`. Claude login policy and LOG_DRIVEN_WORKER
  marker verified through the stub executable. No real model ran in this probe.
- Cursor and Claude login status verified; real model invocation still pending.
- Changed-file whitespace and formatting checks pass.
- Doctor installs the current Claude guard. It still reports inherited
  historical strict-validation failures; history is preserved rather than
  rewritten to hide them. Cursor/Codex repo config directories are absent.

## Recovery sequencing

Before activation, record the complete pending backlog as a fresh result
with exact paths, including the new adapter/config/briefs for isolated
commit-command bootstrapping. Restore a truthful compatibility committed ack
through result 471 with the 16 verified refs. Explicitly supersede later
coordination-only results 512/515/522 with the fresh exact-path result before
resuming past 528; do not advance past the new result or pending worker work.
Bring up the committer first and verify its repair commit, then activate docs
so the historical recovery and new commit triggers cannot race the initial
publication. Verify real Composer/Sonnet behavior, exact commits, docs-loop
suppression, resume and repeated layout/setup convergence.

## Inherited artifact manifest

- `.context/DECISIONS.md` — SHA-256 `64da4d2fdd90e18a1d248e5a329d6bcf6c9dfd1c7d71fc67a68c578900b25081`
- `.context/handoffs/audit-herdr.md` — SHA-256 `64b3307e005cb9c4bc27ac7aeb72d01a0496bfbb8eb4152b7b82062783c3e555`
- `.context/handoffs/audit-partial.md` — SHA-256 `c05fc5dbf74948996ce4d87a9e56dbf9a18daac177e69bd7f45130eff57f6446`
- `.context/handoffs/audit-planner.md` — SHA-256 `545fd99cbad9b63b3b133e3e07c8b597d6facc6d51f5f21750a138ba7dd33fe0`
- `.context/handoffs/audit-state.md` — SHA-256 `1a4c1106fb666c9382fdc87028fdc7ae4b1fa5f07b6bf95e34fcfd41a0312c43`
- `.context/handoffs/eventlog-migration.md` — SHA-256 `b9446869fa62579a79ff82cdbaf26cbdc62e9ec7bf2146a5f7d1675de0154590`
- `.context/handoffs/eventlog-review.md` — SHA-256 `7919eaf02f66f0f333810641f1afe1c4fde106a4c39ad0245a48183c11baf576`
- `.context/handoffs/eventlog-runtime.md` — SHA-256 `0e6e0734154096e546457df99b4adcd05eddd59ea4ce024fedc789fc1bb443a5`
- `.context/handoffs/pr25-prune-state.md` — SHA-256 `a5075b9c369e0550a33ab6789212f356b91276d43ef5e2187df80af51da0c683`
- `.context/handoffs/pr26-root-tab.md` — SHA-256 `99a2728992ca31a2fb1597db27d3992fa1cd809242c4fad658efff44e2e99a06`
- `.context/handoffs/pr29-down-prune.md` — SHA-256 `5ec4a95502bcb38bcb022fa87a5b01526fd2e27b3cd085e51943ba9a8b182c03`
- `.context/handoffs/pr31-live-plan.md` — SHA-256 `bb453b207952146be15d0fbd1b7f5b663f68ef403cb773dd71020dfda8e4e18e`
- `.context/handoffs/pr32-apply-progress.md` — SHA-256 `4802fe9fb16a046a6bf8cd9ba2bf9eb7f705cfa131f3d8c3c36116b0446f7b57`
- `.context/handoffs/pr33-planner-truth.md` — SHA-256 `378d99049679d40784cbbabd1bc1e608185e13e1f0e940db2d433618dd4fc24b`
- `.context/handoffs/pr34-identity.md` — SHA-256 `430e2efc7834504480dfc96f14476e111c2263e9038a1bf98a9eb7744d4ae064`
- `.context/reports/audit-herdr.md` — SHA-256 `7424b3f598ec2affe728055e69660eab459bd3793ec248817dc332166ea1e327`
- `.context/reports/audit-partial.md` — SHA-256 `a02098b35d569127dd97ee0b19820c030e4b7d1125c428c560b4e4f12d6df029`
- `.context/reports/audit-planner.md` — SHA-256 `699b64dfaef73a48c546eb5503fb6452efe2709984eb262ad51ab1cc831e3656`
- `.context/reports/audit-state.md` — SHA-256 `6df46d45c6c548c082f7430b667510ebca1e49d170802caefad2c9bd52791983`
- `.context/reports/audit-summary.md` — SHA-256 `41c5ca236ff4e2e2c219644d331a6887d953ac60ceaa0cc8fc95a203816548de`
- `.context/reports/eventlog-cutover.md` — SHA-256 `c5ede2d584d766e9cda6153c1983c95fe20827febe16eb95f7dfa81fc2d3aa28`
- `.context/reports/eventlog-handoff.md` — SHA-256 `f206d78337b6ab6bff02a2d2f2735596d75846257f9f64f6a1659d9d28e5c8b1`

## Independent configured-command probe

The upstream candidate ran the real Drove model binding with stub CLIs in an
isolated checkout, including new relative adapter/config/brief bootstrap files.
The configured Composer model and full stdin intent were verified. The source
index retained its unrelated staged Cargo.toml patch byte-for-byte after the
repair commit and first docs commit. Evidence: configured-probe under the
snapshot directory, /tmp/drove-consumer-configured-probe.py.

The full loop exposed two upstream blockers and was stopped cleanly:
1. Concurrent committer log append during the second docs pass caused a false
   out-of-scope .context/events.jsonl failure (probe ack537).
2. A no-edit docs pass attempted an empty paths result and failed validation
   (noop-docs.out). Both were sent to the upstream coordinator for product
   fixes; no consumer runtime workaround was introduced.

Thin binding tests also prove propagation of exit7, configured model, event
JSON, commit refs, Claude login selection and worker identity.

Combined Drove all-target/all-feature tests passed: 324 passed, zero failures. Strict Clippy passed. Pane-hook regression tests cover new tabs, creates, splits, restarts, approval/failure gating, retry state, dependencies and adopted caller preservation.

## Live disposable Drove hook verification

The combined debug binary was exercised in a new disposable Herdr session
with an on_start hook that initializes/appends an event before starting
`eventlog view --follow`. The viewer displayed that event; a second plan
had zero actions, a second up was converged, and the hook count remained
one. The disposable session was torn down through Drove. Test script:
/tmp/drove-consumer-hook-smoke.py.

An earlier smoke variant using the Python interpreter as the long-running
serve executable exposed executable-name drift in process inspection; using
the actual native eventlog serve command passed. The production consumer
uses native eventlog serve commands.

Because the accepted pane-hook result is now event528, the pending recovery
batch must explicitly include src/executor.rs and supersede that result too:
use the audited replacement boundary528, not a blanket current-tip baseline.
The fresh exact-path bootstrap result must be above that boundary.

## Accepted consumer verification and activation

Installed eventlog SHA-256:
`7bc864c0654b6a2dd3f35f888dbde90b4e49a9062cd5d665eae43b967aa7baca`.
Accepted upstream refs: c582632, e14c2ec, 1647934, 3da83ed. Upstream reports
220 tests passed, zero failures, one intentional OS-protection skip, plus
strict Clippy and changed-file formatting. Drove release build passed.

Fresh installed-binary configured-probe4 passed actual Drove adapter binding
with stub Composer/Sonnet CLIs, isolated bootstrap additions/deletions, full
stdin intent, both intended documentation updates present in HEAD, exact
committed-or-clean-skipped result accounting, untouched unrelated staging,
and own-doc commit loop suppression. Both native reactors exited and released
their own locks. The initial probe2 deadline was an overly strict expectation
of two commits: both updates had correctly coalesced into one commit.

Installed-binary no-edit docs plus concurrent CLI log append returned skipped
and emitted no result; unrelated staging was preserved. Initial no-edit stub
used an invalid note field; corrected to msg and reran successfully. No local
runtime workaround was added. Evidence: configured-probe4, noop-docs4.out,
configured-probe-events4.json under the snapshot directory.

One-time recovery imports only verified existing IDs: workspaces w1/w2/w3,
six existing tabs, and seven existing panes. Ordered pane membership was
checked against the live snapshot. Initial plan contains four existing-pane
restarts and no creates/closes; controller w1:p1 remains adopted. The first
activation runs the native committer in w2:p2, then full Drove reconciliation
starts docs and the native log viewer after bootstrap commit verification.
