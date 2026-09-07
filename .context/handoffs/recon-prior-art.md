# Brief: recon-prior-art (spawned recon worker)

You were spawned by the Drove controller. Role: read-only reconnaissance. You are NOT the controller.

Hard rules:
- Write exactly ONE file: `.context/handoffs/recon-prior-art-report.md`. Do not edit any other file in this repo or any other repo.
- Never run `git commit`, never touch `.context/events.jsonl`, never start subagents, never message other agents.
- Scratch space if you need it: `/private/tmp/claude-501/-Users-jjmartin-Development-Drove/97ba7d22-31ef-4bde-ad62-eca25a35ad1a/scratchpad/recon-prior-art/`.
- Keep the report under 300 lines. Be concrete: cite file paths, commands, JSON shapes, and quote short snippets. No filler.
- When the report is written, stop and reply with the single line: `REPORT READY: .context/handoffs/recon-prior-art-report.md`

## Task

Survey prior art for a declarative, versioned terminal-workspace file, and extract ergonomic ideas for Drove's Starlark DSL. Use web search if you have it; otherwise work from knowledge and say so.

Cover: Tilt (Tiltfile: `local_resource` with `cmd`/`serve_cmd`, `readiness_probe`, `resource_deps`, `labels`, `auto_init`, `trigger_mode`, `uibutton`/`cmd_button`, `config.define_*`, `load('ext://...')`, `watch_file`), process-compose, overmind/Procfile, mprocs, zellij layout KDL (including `pane_template`, `tab_template`, swap layouts), tmuxinator and tmuxp, devcontainer.json, docker compose (`depends_on` with `condition`, `healthcheck`, profiles, `extends`), Bazel/Starlark macro and rule conventions (kwargs, `select()`, `name` as the one required attr), and Pulumi/CDK component-style composition.

Report:
1. For each tool: a 3-line summary of its model, then the 2–3 ideas most relevant to a terminal-workspace DSL, each with a real syntax snippet.
2. How each handles: naming (one name vs id+label), lists vs trees for layout, long-running vs one-shot processes, readiness and dependencies, reuse/templates, profiles/variants, host-specific overrides, and "the thing that is already running" (adoption).
3. A ranked shortlist of 10 ideas to steal, each with a 5–15 line Starlark sketch of how it could look in a Drovefile.
4. Anti-patterns these tools are criticized for that Drove should avoid.
