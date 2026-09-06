# PR workflow: who drives a pull request to completion

Every PR has one **worker** who owns it until merge, one **reviewer** who owns the verdict, and the **controller** who merges. Nobody else needs to be pinged. The definition of done is the same for all three:

> All review threads resolved, the reviewer's latest review is APPROVE, every required check is green, and the branch is up to date with `main`.

Automated reviewers post on every PR: **cubic** (`cubic-dev-ai`, forced with a comment of `@cubic-dev-ai review this` when the diff is over 2,000 lines) and **CodeRabbit** (`coderabbitai`). Their comments are input, not verdicts.

## Worker (owns the PR)

After `gh pr create`, do not stop. Loop until done:

1. Watch for new input every 60 seconds:
   ```
   gh pr view <n> --json reviews,comments --jq '{r: [.reviews[] | {a: .author.login, s: .state}], c: [.comments[] | {a: .author.login, b: .body[:80]}]}'
   gh api repos/radiator-engineering/Drove/pulls/<n>/comments --jq '.[] | "\(.id) \(.user.login) \(.path):\(.line) resolved=\(.in_reply_to_id != null) \(.body[:120])"'
   gh pr checks <n>
   ```
2. For every inline comment from any author: fix it and push, or reply on that thread with one line saying why not. Then resolve the thread (`gh api graphql` `resolveReviewThread`, or reply and let the reviewer resolve). A thread with no reply is unfinished work.
3. For a review with state CHANGES_REQUESTED: address every bullet, push, and post one comment `Addressed: <bullets>` so the reviewer knows to re-review.
4. For a red check: read the log with `gh run view <id> --log-failed`, fix, push.
5. Keep the branch current: `git fetch origin && git rebase origin/main` when GitHub reports it is behind, then force-push your own branch.
6. When the definition of done holds, reply to the controller with `PR DONE: <url>` and stop. Never merge. If you cannot make progress, reply `BLOCKED: <one line>`.

## Reviewer (owns the verdict)

1. Review per `.context/handoffs/pr-review-template.md`, including the cubic and CodeRabbit comments.
2. After posting CHANGES_REQUESTED, stay alive. Poll every 60 seconds for a new push (`gh pr view <n> --json headRefOid`) or an `Addressed:` comment, then re-review the delta and post a fresh review.
3. Before APPROVE, check: every inline thread has a fix or a reply you accept; checks are green; the branch is not behind `main`. If a thread is unanswered, say which in a CHANGES_REQUESTED review instead.
4. On APPROVE, reply to the controller with `APPROVE: <url>` and stop.

## Controller

Merges with `gh pr merge <n> --squash --delete-branch` once the definition of done holds, records `result` and `retire` for both agents, closes their workspaces and removes the worktree, then starts the PRs that depended on it.
