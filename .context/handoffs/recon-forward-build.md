# Brief: recon-forward-build (spawned research worker)

You were spawned by the Drove controller. Role: read one paper and map its ideas onto Drove. You are NOT the controller.

Hard rules:
- Write exactly ONE file in this repo: `.context/handoffs/recon-forward-build-report.md`. Do not edit any other file here.
- Never run `git commit`, never touch `.context/events.jsonl`, never start subagents, never message other agents.
- Scratch space: `/private/tmp/claude-501/-Users-jjmartin-Development-Drove/97ba7d22-31ef-4bde-ad62-eca25a35ad1a/scratchpad/recon-forward-build/` (create it). You may also use `/tmp` on the DGX.
- Keep the report under 350 lines. Concrete, no filler.
- When done, stop and reply with the single line: `REPORT READY: .context/handoffs/recon-forward-build-report.md`

## The paper

"Forward Build Systems, Formally" (Spall, Mitchell, Tobin-Hochstadt), arXiv 2202.05328. PDF: https://arxiv.org/pdf/2202.05328 (mirror: https://www.alphaxiv.org/pdf/2202.05328).

Parse it with docling on the user's DGX so formulas and figures come through. The host is on the tailnet:

    ssh j-j-m@radiator-dgx 'mkdir -p /tmp/fbs && cd /tmp/fbs && curl -sL -o paper.pdf https://arxiv.org/pdf/2202.05328 && docling paper.pdf --to md --output /tmp/fbs'
    scp j-j-m@radiator-dgx:/tmp/fbs/paper.md /private/tmp/claude-501/-Users-jjmartin-Development-Drove/97ba7d22-31ef-4bde-ad62-eca25a35ad1a/scratchpad/recon-forward-build/paper.md

If that fails, a local docling CLI exists at `~/Library/Python/3.9/bin/docling`. If both fail, read the PDF directly and say so.

## Context: what Drove is

Read `docs/spec.md`, `docs/drovefile.md`, `src/dsl.rs` (the Starlark prelude is the DSL), `src/planner.rs`, `src/executor.rs`, and `~/Development/drove-log-workspace-demo/Drovefile`. Drove is a versioned, declarative description of a terminal workspace (workspaces, tabs, panes, agents, long-running commands, one-shot bootstrap tasks). It plans desired-vs-observed and reconciles on demand, owning only what it created. The user wants it to become as generic as Tilt but with a cleaner DSL, and to have Herdr and a second tool (Radiator) as backends.

## Report

1. The paper's model in your own words: forward scripts, traces, hazards (read-write, write-write, speculation), early cutoff, parallelism, the correctness definition ("same as sequential execution"), and the Rattle/Fabricate/Memoize comparison. Reproduce the key definitions and the main theorems precisely, including any formula docling recovered.
2. The mapping onto Drove. Treat a Drovefile as a forward script whose "commands" are backend operations (create workspace, create tab with layout, run command in pane, start agent, prompt agent, run bootstrap task). What are the reads and writes of each? What is a hazard here (two declarations claiming one pane, a rename racing a create, a bootstrap task that mutates files a pane command reads)? What does early cutoff mean (an already-in-sync resource is skipped) and what evidence would Drove need to record for it (the equivalent of Rattle's trace: digests, observed ids)?
3. Where the analogy breaks: build outputs are files, but workspace resources are live processes with PTYs; "re-running" a pane is destructive; the backend, not the filesystem, is the state store; adoption of an already-running pane has no build-system analogue.
4. A concrete sketch of what a forward-style Drovefile could look like in Starlark (sequential calls with implicit dependencies) versus the current fully declarative one, with the demo Drovefile rewritten in the forward style. State the trade-offs honestly: what the user gains in ergonomics and what Drove must then compute or record.
5. Three to five specific design recommendations for Drove, each one sentence plus a justification tied to a result in the paper.
