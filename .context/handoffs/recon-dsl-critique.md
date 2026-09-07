# Brief: recon-dsl-critique (spawned recon worker)

You were spawned by the Drove controller. Role: read-only reconnaissance. You are NOT the controller.

Hard rules:
- Write exactly ONE file: `.context/handoffs/recon-dsl-critique-report.md`. Do not edit any other file in this repo or any other repo.
- Never run `git commit`, never touch `.context/events.jsonl`, never start subagents, never message other agents.
- Scratch space if you need it: `/private/tmp/claude-501/-Users-jjmartin-Development-Drove/97ba7d22-31ef-4bde-ad62-eca25a35ad1a/scratchpad/recon-dsl-critique/`.
- Keep the report under 300 lines. Be concrete: cite file paths, commands, JSON shapes, and quote short snippets. No filler.
- When the report is written, stop and reply with the single line: `REPORT READY: .context/handoffs/recon-dsl-critique-report.md`

## Task

Critique the ergonomics of the current Drovefile DSL and sketch alternatives. Do not decide; present options with trade-offs. The controller decides.

Read: `docs/spec.md`, `docs/drovefile.md`, `docs/migration.md`, `src/dsl.rs` (the Starlark prelude is the DSL), `src/model.rs`, `src/planner.rs`, `examples/basic/Drovefile`, and the real-world attempt at `~/Development/drove-log-workspace-demo/Drovefile`. Also read `~/.claude/skills/setup-log-driven-workspace/SKILL.md` and `scripts/layout.sh` there: that imperative script is the workflow Drove must replace declaratively.

Goal statement from the user: "the most ergonomic tool, the missing piece Herdr doesn't have; as generic as Tilt but cleaner than the current DSL; Herdr is one backend, Radiator is another; able to set up a pane that runs an agentic workflow."

Report:
1. Pain points, each with a concrete excerpt from the demo Drovefile showing it. Cover at least: id/label duplication; binary `split(first, second, ratio)` trees for what are really lists; agents declared apart from the pane they live in; commands as argv arrays only; no notion of the invoking pane ("this pane is the controller, adopt it"); no readiness, dependency or ordering between panes; no reusable presets beyond `load()`; profiles as whole-workspace lists; anything else you see.
2. Hidden assumptions in the model and planner (ownership by logical id, tab replacement destroying PTYs, detach semantics, layout comparison by normalized JSON, agents matched by kind only).
3. For each pain point, one or two alternative Starlark syntaxes, each with the demo excerpt rewritten. Keep Starlark; show how the same declaration reads under each option. Include at least one option that models a workspace as a flat list of named "resources" with placement hints (Tilt-like) and one that keeps explicit topology but with lighter syntax.
4. A rewritten demo Drovefile in your preferred option, in full, for a side-by-side line count.
5. Things the current design gets right that should survive.
