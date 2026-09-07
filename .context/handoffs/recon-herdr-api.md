# Brief: recon-herdr-api (spawned recon worker)

You were spawned by the Drove controller. Role: read-only reconnaissance. You are NOT the controller.

Hard rules:
- Write exactly ONE file: `.context/handoffs/recon-herdr-api-report.md`. Do not edit any other file in this repo or any other repo.
- Never run `git commit`, never touch `.context/events.jsonl`, never start subagents, never message other agents.
- Scratch space if you need it: `/private/tmp/claude-501/-Users-jjmartin-Development-Drove/97ba7d22-31ef-4bde-ad62-eca25a35ad1a/scratchpad/recon-herdr-api/`.
- Keep the report under 300 lines. Be concrete: cite file paths, commands, JSON shapes, and quote short snippets. No filler.
- When the report is written, stop and reply with the single line: `REPORT READY: .context/handoffs/recon-herdr-api-report.md`

## Task

Enumerate the Herdr socket API and compare it with what Drove uses today.

1. Run `herdr api schema --json` and save it to your scratch dir. List every method with its params and result shape in a compact table (method, required params, optional params, one-line result).
2. Read `src/herdr.rs` (this repo). List the methods Drove calls and how (params it passes).
3. For each of these, say exactly what the API supports, with the JSON shapes:
   - creating a workspace with cwd, label, env
   - applying a whole tab layout in one call (does it accept pane commands, cwd, env, labels? ratios?) and what `export layout` returns (does it include commands, labels, cwd, agent info?)
   - running a command in a pane vs starting a pane with a command
   - `agent start` options and kinds; whether an initial prompt can be sent; how `agent prompt` waits
   - worktree create/open
   - `report-metadata` tokens and titles on workspaces and panes (what are they for?)
   - notifications
   - anything about the caller's own pane (`HERDR_PANE_ID`): can an existing pane be adopted, relabeled, moved into a new tab or workspace?
   - moving panes between tabs/workspaces; closing
   - named sessions and `--remote`
4. Capabilities Drove does not use today that matter for declaring a workspace, ranked by usefulness, one line each.
5. Constraints or gotchas you found (ID stability after move, layout replacement destroying PTYs, timeouts, agent detection latency).
