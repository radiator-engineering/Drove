# Drove consumer repair — 2026-09-07

The native eventlog consumer is installed and active in the existing Drove
session. History, inherited work, and the adopted controller were preserved.
The final backend repair and this report are published through the exact-path
controller result and native commit acknowledgment in the log.

## Installed components and ownership

Eventlog installed SHA-256:
`7bc864c0654b6a2dd3f35f888dbde90b4e49a9062cd5d665eae43b967aa7baca`.
Accepted upstream implementation refs: c582632, e14c2ec, 1647934, 3da83ed.
Upstream passed 220 tests, zero failures, one intentional OS-protection skip,
strict Clippy and changed-file formatting. The version string remains 0.1.0.

Drove consumes upstream setup, generated helper, lifecycle, react, and actions.
Its small model-command.py binding invokes Composer 2.5 Fast and Claude Sonnet
with the complete driving event. Eventlog owns scope, isolated commit creation
and publication, index protection, timeout, lock, outcome and docs-loop logic.
Removed local reactor/supervisor scripts are retained in Git history.

The Drove controller owns this repository and its log. The upstream controller
owns event-log and does not write this log. Existing IDs were verified against
the live snapshot, including ordered pane membership, before a one-time state
import: workspaces w1/w2/w3, six tabs, seven panes. Controller w1:p1 remains
adopted. No duplicate workspace, tab or pane was created; every apply used --no-focus.

Current panes: controller w1:p1; native log viewer w1:p2; agentmon w1:p3;
lazygit w2:p1; native committer w2:p2; native docs w2:p3; files w3:p1.
The labels of the existing docs/files tabs and reactor panes were reconciled.

## Audited recovery and real model outcomes

- All 22 inherited dirty artifacts were preserved byte-for-byte; DECISIONS.md
  retains its original prefix plus new decisions. The manifest below records
  the inherited hashes. Full history and source snapshots remain in the
  evidence directory.
- All 16 historical commit refs were verified ancestors of HEAD. Recovered
  committed ack531 maps result471 and restores the missed cumulative docs
  trigger without pretending historical commands ran again.
- Ack532 explicitly supersedes pending coordination results512/515/522 and
  accepted hook result528 with fresh exact-path bootstrap result530. External
  global-config result523 has no repository scope. No current-tip baseline
  hid pending work.
- Composer processed all 56 paths in result530. Ack535 reports
  `7213e44ec6029764840cbc14bc9f9ae21ad302cf`,
  `f73659f04d995c424d213d93640711ee20d0afa5`, and
  `7170bcb247eff480cdf7d2f26ff9939d4f271a02`. Their changed-path union exactly
  equals the authorized set; source and index were clean afterward.
- Sonnet documented the bootstrap hook behavior in result540; ack541 reports
  updated. Composer committed it as
  `8022a9bf36f5168ba735122ec57ad48a57e3c7dc` in ack543. Own-doc ack545 skipped,
  proving loop suppression with real models.
- Historical docs attempt538 failed because the controller created an ignored
  worktree inside the repository during the docs snapshot. The guard correctly
  rejected those outside-root additions; Sonnet itself found no docs changes.
  Once the active pass settled, the worktree was moved outside the repository.
  The same verified refs were requeued at546; native docs ack548 reports
  skipped/no documentation changes. The original failure remains visible.

## Drove defects found through live verification

The first accepted fix runs pane on_start hooks before commands and preserves
retryable dependency failures. Worker drove-pane-hooks owned src/executor.rs,
was reviewed and accepted at528, retired at529, and its workspace was closed.
Its uncommitted checkout is preserved externally at
`/Users/jjmartin/Development/Drove-worktrees/consumer-pane-hooks`.

Live cutover exposed a separate backend defect: restart_command only typed
argv into a running program. The repaired Herdr backend interrupts once,
waits up to ten seconds for a verified shell, then atomically submits input.
Busy jobs receive no replacement text and remain retryable. Foreground process
inspection selects the group leader, avoiding false drift from lazygit's
short-lived Git children.

Herdr 0.8.2 pane run is CLI sugar for pane.send_input, not a replacement API.
Its shell_pid field can also name a directly launched program. New Drove
root/tab serve panes therefore retain an interactive shell, like split panes.
An older direct-exec pane without a shell is explicitly refused for in-place
restart; it is never treated as a shell or silently replaced. The existing
seven production panes have shell parents and retain their IDs.

The backend checkout is preserved outside the repository at
`/Users/jjmartin/Development/Drove-worktrees/consumer-restart-command`.
Neither retained checkout runs an agent or reactor.

## Production resume and final installation

Installed Drove SHA-256:
`9246868482f2a2418369e9b411d758ee6af002cc0f0a21f0eb37896dcc63b6f6`.
Final release build passed. While both reactors were idle, graceful interrupts
returned their panes to shells and released their own locks. Native lifecycle
stop retired them at552/553. Drove up executed the native on_start hooks,
restored spawn/claims at554–557, and restarted exactly two existing panes.
Both runtime commands were verified through process_info. Checkpoints remained
committer540 and docs546, with no replay or open intents. A subsequent plan
had zero actions, up reported already_running, and setup upgrade reported no
changes. The final live snapshot still contains exactly the original three
workspaces, six tabs, seven panes, and adopted controller.

The final source/result publication follows those proofs. Its actual commit
refs and the resulting docs outcome are recorded by native reactors in the
append-only log; the controller does not make manual production commits.

## Verification

- Final combined Drove suite: 331 tests passed, zero failures. Strict
  all-target/all-feature Clippy and changed-file formatting passed.
- Installed eventlog configured-probe4 used the real Drove adapter with stub
  model CLIs. Bootstrap additions/deletions, models, full stdin intent, both
  intended docs edits in HEAD, exact committed-or-clean-skipped outcomes,
  unrelated staged patch preservation, own-doc suppression and lock cleanup
  passed. Valid coalescing can put two docs results into one commit.
- No-edit docs with concurrent CLI append returned skipped, emitted no result
  and preserved staging. No local workaround for upstream scope rules exists.
- A live disposable hook probe confirmed initialization before the native
  viewer, one hook on first start, and zero-action repeat.
- The final live restart probe confirmed native viewer replacement with a new
  PID in the same pane, exact hook counts, stable PID and zero-action repeat.
  An INT-ignoring bash job timed out with no replacement input, unchanged root
  PID and a retryable plan. Its disposable session was torn down.

Evidence root:
`/var/folders/x9/rz97sz4s75z45ym82kdfz87r0000gn/T/drove-consumer-nit8dild`.
Key output: `/tmp/drove-restart-shell-tests.out`,
`/tmp/drove-restart-shell-clippy.out`,
`/tmp/drove-consumer-restart-final-smoke.out`,
`/tmp/drove-consumer-direct-root-smoke.out`.
Original log/source snapshots, manifests, recovery refs, ownership maps,
model captures and bootstrap acknowledgment are in the evidence root.

Historical strict-doctor violations remain visible; history is not rewritten
to hide old missing claims, duplicate spawns or conflicts. No log was erased
or manually rewritten, and no reactor lock directory was manually removed.

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

