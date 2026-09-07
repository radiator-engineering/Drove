# Brief: recon-radiator (spawned recon worker)

You were spawned by the Drove controller. Role: read-only reconnaissance. You are NOT the controller.

Hard rules:
- Write exactly ONE file: `.context/handoffs/recon-radiator-report.md`. Do not edit any other file in this repo or any other repo.
- Never run `git commit`, never touch `.context/events.jsonl`, never start subagents, never message other agents.
- Scratch space if you need it: `/private/tmp/claude-501/-Users-jjmartin-Development-Drove/97ba7d22-31ef-4bde-ad62-eca25a35ad1a/scratchpad/recon-radiator/`.
- Keep the report under 300 lines. Be concrete: cite file paths, commands, JSON shapes, and quote short snippets. No filler.
- When the report is written, stop and reply with the single line: `REPORT READY: .context/handoffs/recon-radiator-report.md`

## Task

Drove (this repo) is a versioned, declarative description of a terminal workspace. Today its only backend is Herdr (workspace → tab → pane → agent). The user's other tool, **Radiator**, will be a second backend, and Radiator will ship a tool that generates Drove configurations server-side.

Explore these repos (read-only): `~/Development/radiator-cli`, `~/Development/radiator-neue`, `~/Development/radiator-engineering`, and `~/Development/radiator-std-snapshot` if the first three are thin. Skim READMEs, docs, CLI entry points, config/manifest formats, and any socket/IPC/API.

Report:
1. What Radiator is, in a paragraph, and its current state (prototype, in use, abandoned).
2. Its layout model: the nouns (panel, panes, sessions, views, whatever they are) and how they nest. A table mapping Herdr's `workspace / tab / pane / agent / worktree` to Radiator's nearest equivalent, with "none" where there is none.
3. Radiator-only concepts a shared Drove model must accommodate (for example server-side rendering, remote hosts, panels bound to data sources, non-terminal panels).
4. Any existing config/manifest format Radiator reads, with a real excerpt.
5. Any evidence about "server-side-constructed configurations": what generates what, for whom.
6. Where a Drove backend would plug in: the API or CLI calls that create/list/rename/close layout objects and run commands in them, with signatures if they exist.
7. Open questions you could not resolve from the code.
