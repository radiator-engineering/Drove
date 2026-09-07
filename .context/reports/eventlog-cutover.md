# Eventlog migration and cutover, 2026-09-07

## Baseline

Main started at `1b71bf0` (v0.1.2), with product files clean. Inherited changes are `.context/DECISIONS.md`, audit/PR handoffs, and audit reports. They are prior coordination work, not new product edits.

Legacy committer last recorded ack: event 350, covering result 346. Its Herdr output shows it continued processing events and committed `1b71bf0` for result 471, but could not record acknowledgments because `/Users/jjmartin/.local/bin/append-event.sh` was missing. Legacy doc worker last ack: event 351, covering committer ack 350. Missing committer acknowledgments prevented further doc passes.

The controller verified both old reactors had no model action in flight, sent Ctrl+C to their existing panes (`w2:p2`, `w2:p3`), and verified both returned to their shell prompts. Their own cleanup handled locks. Event 479 records the stop. No shell removes a lock or edits the log.

## Historical lifecycle repair

The live session had no historical worker processes or worktrees. These old open log records are malformed duplicate names or superseded reviewers; their normal records already completed:

| Open record | Evidence of completion |
| --- | --- |
| `pr2-planner v2/pr2-planner` | result 179, retire 181 |
| `pr3-herdr-backend v2/pr3-herdr-backend` | result 211, retire 212 |
| `pr5-radiator-backend v2/pr5-radiator-backend` | result 191, retire 195 |
| `recon-dsl-critique claude sonnet w8` | result 55, retire 61 |
| `recon-herdr-api cursor composer-2.5-fast w5` | result 51, retire 58 |
| `recon-prior-art cursor composer-2.5-fast w7` | result 48, retire 60 |
| `recon-radiator cursor composer-2.5-fast w4` | result 53, retire 57 |
| `recon-setup-skill cursor composer-2.5-fast w6` | result 47, retire 59 |
| `review-pr6` | replacement `review-pr6b` retired at 222 |
| `review-pr7` | replacement `review-pr7b` retired at 226 |

Repair appends a result without file paths (no new change to commit) and a retirement for each exact old name. Existing events remain unchanged.

## Prefix audit and recovery plan

The controller checked `git status --porcelain --untracked-files=all -- <paths>` for every result in `(346, 471]`. Product paths are clean. Results 367/371/377/393 name inherited dirty decisions or handoffs; results 415/417/419/423 name untracked audit reports. Result 367 names the entire handoff directory, which now also contains newer work. Therefore blind replay is unsafe even with path scoping.

Sol independently confirmed that the native runtime resumes from the maximum `seq_done`, recognizes legacy acknowledgments, and does not automatically baseline an existing reactor identity. It also confirmed that turning the already-completed release result into a new skipped outcome would lose its missing doc trigger. The planned recovery is:

1. Preserve inherited coordination artifacts and explicitly report their backlog using exact file paths, separate from historical broad-directory results.
2. After the implementation is reviewed and landed, restore a truthful compatibility acknowledgment for the verified release completion through result 471. Identify it as recovered from terminal output and Git history, never as newly executed work.
3. Restore the missing documentation trigger, include the missed commit interval in the recovery evidence, grant the doc worker its live claims, and verify its actual result/ack chain.
4. Resume the committer above the audited prefix and verify scoped commits plus the doc-loop guard. Do not suppress failures or pending new results by advancing to the current log tip.

`eventlog react test` only suppresses runtime log appends: its action still executes. All action tests run in disposable repositories.

## Verified recovery mapping (controller, 2026-09-07)

Git history and changed-path lists were inspected on main at `1b71bf0`.
Every commit below is reachable from main. The recovery acknowledgment will
cover results through 471 and name the unique commits in Git ancestry order.
It restores missing evidence of shipped work; it does not claim a new pass ran.

| Result | Reachable commit | Scope disposition |
| --- | --- | --- |
| 354 | `3b4c07f` | Shipped product changes; clean on main. |
| 359 | `15ab054` | Shipped product changes; clean on main. |
| 367 | `3fbf55a` | Release and historical briefs shipped; later dirty decisions/handoffs require a fresh exact-path result. |
| 371 | `d722844` | D47 spec and brief shipped with PR 28; later decision additions are separate pending work. |
| 377 | `9b60e93` | D48/D49 spec shipped; decision additions and PR 25/26 briefs remain pending. |
| 390 | `d722844` | Shipped product changes; clean on main. |
| 394 | `bf2589f` | Shipped product changes; clean on main. |
| 397 | `5fe839d` | Shipped product changes; clean on main. |
| 400 | `ca1a3e2` | Shipped product changes; clean on main. |
| 414 | `6611b33` | Shipped product changes; clean on main. |
| 425 | `650355e` | Shipped product changes; clean on main. |
| 443 | `0e6772a` | Shipped product changes; clean on main. |
| 450 | `1dd0f17` | Shipped product changes; clean on main. |
| 454 | `6a453e2` | Shipped product changes; clean on main. |
| 461 | `22a69f3` | Shipped product changes; clean on main. |
| 467 | `e40234c` | Shipped product changes; clean on main. |
| 471 | `1b71bf0` | Shipped product changes; clean on main. |

Results 393, 415, 417, 419 and 423 are coordination-only pending work,
to be recovered with fresh exact-path results before advancing the checkpoint.
Review results 427, 452, 456, 463 and 469 name the unchanged review template;
there is no file delta or missing product commit for those results.
The remaining events in (346, 471] are not results and require no commit action.

The last acknowledged spec commit `c4ebe31` survives as a Git object but its
reachable counterpart is `4bf513a`; result 346 was already acknowledged and
its doc pass skipped at 351, so it is excluded from the recovered commit list.
Result 354 is included even though its commit precedes that rewritten spec in
Git ancestry: log order and rebased Git order differ.

Recovery refs (unique, oldest first):

```text
3b4c07fc885b667374c39e3986346e70c197422c,15ab0542d1c212095b126b3144bf001dc7690f84,3fbf55a838f49d1baf1966710b599beb78c7dd51,d72284447e7defbf3772fe1cc538c7fba7029e8c,9b60e932210a11b6ed2c4ea8fda6a9d764ed3d34,bf2589f24a161cae3e0b748b4d02afe66086ba56,5fe839dc5edf385ee940e9ec0239a1a6214f6da4,ca1a3e276ac8cdabd95d712dd871d398011e51eb,650355efd8ebd726c7f7e8455eec934d260e71dc,6611b33c6de4a62aa45d63e66483b1c6bb1c28e9,1dd0f1782eead1a1a181dc4e0a3d93c3506d86af,0e6772a8e779feb7ad2fb0848f1c1ef2cc4361b2,6a453e23d602f564ccb28419fc5c4ee4f2d8982e,22a69f3cb4bd30369f7a5b318e0f18ed9649fdb7,e40234c5fa916b991d41dccd1a4ebca8a24e0032,1b71bf005eb0657300b206a9d3c71b96bf1dfb55
```
