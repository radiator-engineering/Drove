# Brief: recon-setup-skill (spawned recon worker)

You were spawned by the Drove controller. Role: read-only reconnaissance. You are NOT the controller.

Hard rules:
- Write exactly ONE file: `.context/handoffs/recon-setup-skill-report.md`. Do not edit any other file in this repo or any other repo.
- Never run `git commit`, never touch `.context/events.jsonl`, never start subagents, never message other agents.
- Scratch space if you need it: `/private/tmp/claude-501/-Users-jjmartin-Development-Drove/97ba7d22-31ef-4bde-ad62-eca25a35ad1a/scratchpad/recon-setup-skill/`.
- Keep the report under 300 lines. Be concrete: cite file paths, commands, JSON shapes, and quote short snippets. No filler.
- When the report is written, stop and reply with the single line: `REPORT READY: .context/handoffs/recon-setup-skill-report.md`

## Task

Produce a capability inventory of the imperative workflow Drove must be able to express declaratively.

Read all of `~/.claude/skills/setup-log-driven-workspace/` (SKILL.md, scripts/*.sh, templates/*), plus `~/.claude/skills/event-log-coordination/scripts/*.sh` and `~/.claude/skills/herdr-layouts/scripts/*.sh` where the setup scripts call them. Also read this repo's `docs/migration.md`, which is a first attempt at the same mapping.

Report:
1. A complete table of every side effect the skill has, in execution order: files written (tracked vs untracked/ignored), hooks registered (git hooks, Claude Stop hook, PreToolUse guard), gitignore lines, OS protection, decision events, spawn/prompt/claim/retire events, workspaces/tabs/panes created with their labels and commands, reactors started (with supervisor, lock dir, resume-from-ack logic), the agentmon `--since` cutoff computation, discovery/reuse rules in layout.sh (how it decides a tab already exists), and teardown.
2. Classify each row as one of: (a) tracked file scaffolded once, (b) machine-local bootstrap with idempotent check/run, (c) long-running pane process, (d) agent to start and then prompt, (e) event to record in the log, (f) adoption of the invoking pane, (g) something else (say what).
3. Parameters the user can vary (models, doc roots, budgets, churn files, protect) and where they live.
4. Which rows the current Drovefile (see `docs/drovefile.md`) cannot express, and precisely why (missing noun, missing lifecycle, wrong lifecycle).
5. The reuse/idempotency contract layout.sh implements, stated as rules, since Drove's planner must match or beat it.
