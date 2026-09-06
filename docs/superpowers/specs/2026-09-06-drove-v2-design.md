# Drove v2 design

**Date:** 2026-09-06
**Status:** Decided by the controller from six recon reports; open for user override.
**Inputs:** `.context/handoffs/recon-*-report.md` (setup-skill inventory, prior art, Herdr API, Radiator, DSL critique, forward-build paper).

## 1. Goal

Drove is the versioned description of a developer's whole agent workspace. One
file in the repo, one command, and the workspace exists: panes, long-running
processes, agents with their prompts, one-shot setup tasks, and the invoking
pane adopted as the controller. Herdr is the first backend. Radiator's hub is
the second. Radiator will generate Drove documents server-side.

Success looks like this: everything `setup-log-driven-workspace` does today
with `setup.sh`, `layout.sh`, `status.sh` and `teardown.sh` is one Drovefile
plus `drove up`, `drove status`, `drove down`.

## 2. Decisions

Each decision names its evidence. Change a decision by editing this section
and recording it in `.context/DECISIONS.md`.

| # | Decision | Why |
|---|---|---|
| D1 | The file format stays declarative Starlark that builds values. No forward-style script as the authoring format. | Forward style is better for generation and worse for review; identity would have to be derived from call sites, which turns a reorder into a silent ownership change (forward-build report §4). Spec constraint: stable review diffs. |
| D2 | The planner adopts the forward-build model internally: every action declares reads and writes, a whole-plan hazard pass runs before execution, observed digests are recorded per resource, destructive actions are never reordered or parallelized. | Rattle's real bug came from per-command hazard checks (paper §6.4). Recording writes as well as reads is what makes skipping self-correcting (`script ≡ rattle-unchecked`). |
| D3 | One canonical intermediate representation (IR): JSON, schema version 2, a flat list of typed resources. Starlark compiles to it. Backends consume only it. `drove render` prints it. `drove up --ir file.json` accepts it. | Radiator generates configurations server-side (user brief). Radiator recon found no generator yet; a JSON contract is the seam. |
| D4 | Topology is a placement hint, not the model. The core nouns are workspace, pane, agent, task. Tabs and splits are Herdr placement; a backend without them flattens with a warning. | Radiator hub is workspace → flat panes with no tabs or splits (Radiator report §2). |
| D5 | One positional `name` per resource. `label` defaults to `name`. `name` is identity within its scope. | Demo file: of 18 nodes only 3 needed a label different from the id (DSL critique §1). |
| D6 | A tab lists panes with `split` and `ratios`; binary trees are gone from the surface. | Reordering is a list edit, not a tree rewrite (DSL critique, Zellij and Tilt prior art). |
| D7 | An agent is a property of its pane: `pane(name, agent=agent(kind, args, prompt))`. Drove starts it and sends the prompt. | Agents apart from panes were a string cross-reference; the demo had no agents at all (DSL critique). Herdr has `agent.prompt` (API report §3). |
| D8 | Exactly one pane per profile may declare `adopt="caller"`. Drove never creates, moves or replaces it. It renames the enclosing workspace and tab and splits new panes off it. | The whole imperative workflow is built around the invoking pane (setup-skill report B2). |
| D9 | Herdr `layout.apply` is used only for tabs Drove creates from nothing. Any tab that contains a live pane Drove owns or adopts is converged with `pane.split`, `pane.close`, `layout.set_split_ratio` and `rename`. | Spike on 2026-09-06 in an isolated session: `layout.apply` with a `pane_id` hint still replaced the tab and killed the pane. |
| D10 | `pane(serve=[...])` is the long-running process. `task(name, run, check, inputs, after, auto)` is the one-shot. `auto=True` with `check` is today's bootstrap. `auto=False` is a Tilt-style manual trigger via `drove run <name>`. | One noun for one-shot work; Tilt `cmd` versus `serve_cmd`; Tilt buttons (prior art). |
| D11 | Every argv Drove executes on the host (tasks, hooks) is approval-gated on bytes, as bootstrap is today. Pane `serve` commands run inside a visible pane and are not gated. | Keeps the trust boundary the spec chose. |
| D12 | Resources may declare `ready` (`output("text")`, `port(n)`, `cmd([...])`) and `after=[names]`. Drove starts a resource only after its `after` set is ready. Readiness is re-checked on every reconcile, not only at start. | `layout.sh` waited for `watching` (setup-skill B9). Tilt's once-only startup deps are a named anti-pattern (prior art §4). |
| D13 | Resources may declare `on_start` and `on_stop` argv hooks. Drove runs them once per actual start or stop, in repo root, with `DROVE_RESOURCE` and backend ids in the environment. | This is how spawn, prompt and retire events reach the coordination log without Drove knowing the log exists (setup-skill B10, C2). |
| D14 | `serve=any_of([argv1], [argv2])` picks the first argv whose executable is on PATH at plan time and records the choice. | "agentmon, else htop" and "lazygit, else shell" (setup-skill §4). |
| D15 | Profiles compose by name: `profile("core", extends="default", without=["files"])`, plus `--only` and `--without` on the CLI. | Profiles as whole-workspace lists cannot say "default minus one thing" (DSL critique). |
| D16 | Drove writes ownership tokens on the backend (`drove_name`, `drove_profile`, `drove_repo`) where the backend supports metadata. Discovery reads tokens first, then the local state file, then label plus cwd as a last resort. | Beats `layout.sh`'s label-and-cwd heuristic; survives loss of the state file (API report: `report_metadata`). |
| D17 | Drift on a pane's command is detected from the backend's process info, and a changed command restarts only that pane in place. Agent drift compares the recorded argv digest and the observed kind. | Export omits command and env; agents were matched by kind only (API report, DSL critique §2). |
| D18 | Correctness statement: for every profile, `drove up` either makes the owned resources equal to the sequential application of the declared actions, or exits non-zero with a named hazard or conflict. Unowned resources are outside the relation. | Paper §6.3 weakening; matches the ownership model. |
| D19 | `drove down` runs `on_stop` hooks and closes owned resources, never the adopted pane. It replaces `teardown.sh`. | Setup-skill C1–C3. |
| D20 | No v1 compatibility layer. The repo is one day old; `schema_version` becomes 2 and the v1 prelude is removed. | Cost of a shim exceeds its value. |

## 3. The model

Five resource kinds. All live in one profile-scoped namespace for `after`,
`on_*` and adoption references.

| Kind | Identity | Fields |
|---|---|---|
| `workspace` | `name` | `label`, `cwd`, `env`, `tabs` |
| `tab` | `workspace/name` | `label`, `panes`, `split` (`right` or `down`), `ratios` |
| `pane` | `name` (unique per profile) | `label`, `cwd`, `env`, `serve`, `ready`, `after`, `adopt`, `agent`, `on_start`, `on_stop` |
| `agent` | owned by its pane | `kind`, `args`, `prompt`, `name` |
| `task` | `name` | `run`, `check`, `inputs`, `after`, `auto`, `on_start`, `on_stop` |

Backend capabilities, declared by the backend at connect time and checked
against the IR at plan time:

| Capability | Herdr | Radiator hub |
|---|---|---|
| tabs | yes | no (flatten, warn) |
| splits and ratios | yes | no (ignore, warn) |
| workspace env | yes | unknown, verify |
| pane command at create | yes | yes (`pane.open`) |
| agent start | yes | no (run the agent binary as `serve`) |
| agent prompt | yes | `pane.send_text` |
| adopt caller | `HERDR_PANE_ID` | `RADIATOR_PANE_ID` |
| metadata tokens | yes | title only |
| process info | yes | verify |
| events | `events.subscribe` | `events.subscribe` |

## 4. The DSL

The log-driven workspace, complete. This file replaces `setup.sh --commit`
(tracked scaffold still lands once via a task), `layout.sh`, `status.sh` and
`teardown.sh`.

```python
load("drove/reactors.star", "reactor")

scaffold = task(
    "scaffold",
    check = ["bash", ".context/bin/scaffold.sh", "--check"],
    run = ["bash", ".context/bin/scaffold.sh"],
    inputs = [".context/bin/scaffold.sh"],
)
protect_log = task(
    "protect-log",
    check = ["protect-log.sh", "--status"],
    run = ["protect-log.sh"],
    auto = False,
)

control = workspace("control", tabs = [
    tab("coordinator", split = "down", ratios = [0.5], panes = [
        pane("controller", adopt = "caller"),
        pane("eventlog", serve = ["eventlog-view.sh", "-f"]),
    ]),
    tab("monitor: system + agents", panes = [
        pane("agentmon", serve = any_of(["agentmon", "--since", "launch"], ["htop"])),
    ]),
])

maintenance = workspace("maintenance", tabs = [
    tab("lazygit", panes = [pane("gitlog", serve = any_of(["lazygit"], ["bash"]))]),
    reactor("commit", agent = "cursor-committer", model = "composer-2.5-fast",
            script = "cursor-commit-reactor.sh"),
    reactor("doc-sync", agent = "doc-worker", model = "sonnet",
            script = "doc-sync-reactor.sh", after = ["commit-reactor"]),
])

files = workspace("files", tabs = [
    tab("files", panes = [pane("spiceedit", serve = any_of(["spiceedit"], ["bash"]))]),
])

profile("default", workspaces = [control, maintenance, files], tasks = [scaffold, protect_log])
profile("core", extends = "default", without = ["files"])
```

`drove/reactors.star`:

```python
def reactor(name, agent, model, script, after = []):
    slug = name + "-reactor"
    return tab("{}: {} reactor".format(model, name), panes = [
        pane(slug,
             serve = ["bash", ".context/bin/run-reactor.sh", script],
             ready = output("watching"),
             after = after,
             on_start = ["append-event.sh", "spawn", "agent=" + agent, "model=" + model,
                         "role=" + slug, "runtime=headless"],
             on_stop = ["append-event.sh", "retire", "agent=" + agent, "disposition=stopped"]),
    ])
```

An interactive agent pane, for the "pane that runs an agentic workflow" case:

```python
pane("review", agent = agent("claude", args = ["--model", "sonnet"],
                             prompt = "Read .context/handoffs/review.md and do only that."))
```

Rules the compiler enforces: names match `[a-z][a-z0-9_-]{0,31}`; one
`adopt` per profile; `ratios` has `len(panes) - 1` entries each between 0.05 and 0.95 inclusive;
`after` targets exist and form a DAG; `extends` targets exist; `profile()`
returns the profile value it registers.

## 5. The planner

Every action carries `reads` and `writes` over resource addresses:

| Action | Reads | Writes |
|---|---|---|
| CreateWorkspace | repo root | `workspace/<n>` |
| RenameWorkspace, RenameTab, RenamePane | the resource's runtime id | its `label` |
| CreateTab (fresh, `layout.apply`) | `workspace/<n>` | `tab/<w>/<n>`, every `pane/<p>` in it |
| SplitPane | the neighbour pane's runtime id | `pane/<p>` |
| RestartPane, ClosePane | `pane/<p>` | `pane/<p>` (destructive) |
| SetRatio | `tab/<w>/<n>` | the tab's geometry |
| StartAgent, PromptAgent | `pane/<p>` | `agent/<p>` |
| RunTask | `inputs` | the repo paths the task declares in `writes` |
| RunHook | the resource's ids | nothing tracked |
| Adopt | caller ids | `pane/<p>` binding only |

Plan pipeline: compile → capability check → observe → diff → order (tasks,
then workspaces, tabs, panes in `after` order, agents, hooks) → hazard pass →
approval → execute. The hazard pass rejects, with a named `Hazard` action:
two actions writing one address; a write to an address an earlier action
read, unless the writer is the declared owner; a `RunTask` whose declared
writes overlap a running pane's `cwd` or `inputs`. Destructive actions keep
their relative order and never run concurrently with anything.

Local state records per resource: desired digest, observed digest at apply
time, runtime ids, chosen `any_of` argv, agent argv digest, task and hook
approval digests. Early cutoff: skip a resource whose desired digest is
unchanged and whose observed state still matches its observed digest.

## 6. CLI

```
drove status [--profile P] [--json]        exit 0 in sync, 2 out of sync, 3 backend down
drove plan   [--profile P] [--json]
drove up     [--profile P] [--yes] [--allow-restart] [--only a,b] [--without c]
drove down   [--profile P]                 hooks, close owned, keep adopted
drove run    <task>                        manual task, approval-gated
drove render [--profile P]                 IR JSON to stdout
drove up --ir plan.json                    apply a generated IR
```

Backend selection: `--backend herdr|radiator`, else `HERDR_ENV` then
`RADIATOR_HUB` from the environment.

## 7. Work breakdown

Each row is one PR by one worker in its own worktree and workspace, reviewed
by a separate review agent. Foundation first, then three in parallel.

| PR | Scope | Depends on | Claims |
|---|---|---|---|
| 1 | Model v2, IR schema 2, DSL prelude v2, `drove render`, `Backend` trait with capabilities, docs/drovefile.md rewrite | none | `src/model.rs`, `src/dsl.rs`, `src/ir.rs`, `src/backend/mod.rs`, `docs/drovefile.md`, `examples/` |
| 2 | Planner v2: resource actions with reads and writes, observed digests, hazard pass, early cutoff, `after` ordering, tests | 1 | `src/planner.rs`, `src/state.rs` |
| 3 | Herdr backend v2: incremental tab convergence, adoption, metadata tokens, agent start and prompt, process-info drift, `SetRatio` | 1 | `src/backend/herdr.rs`, `src/herdr.rs`, `scripts/smoke-herdr.sh` |
| 4 | Tasks and hooks: `auto`, `drove run`, `on_start`, `on_stop`, `drove down`, approval of hooks | 2 | `src/bootstrap.rs` → `src/tasks.rs`, `src/hooks.rs`, `src/cli.rs` |
| 5 | Radiator hub backend with capability degradation | 1 | `src/backend/radiator.rs` |
| 6 | Log-driven workspace example, `drove/reactors.star`, `.context/bin/scaffold.sh`, migration doc, end-to-end smoke against the real reactors | 3, 4 | `examples/log-driven/`, `docs/migration.md` |
| 7 | `drove watch` on `events.subscribe` with drift notifications | 3 | `src/watch.rs` |

## 8. Open questions for the user

1. Radiator hub `pane.open` has no readiness or process-info surface today. Should PR 5 wait for hub changes, or ship with `ready` unsupported on that backend?
2. Should `drove up` refuse to run outside a backend pane (no caller to adopt) when the profile declares an `adopt` pane, or create the controller pane instead and warn?
3. Prompt text for agents: inline string, or `prompt_file=` only, so prompts are reviewable files? Current choice: both, inline capped at 2 KB.

## 9. Amendments from the prior-art pass (2026-09-06)

Source: `docs/superpowers/research/2026-09-06-neuroarxiv-drove-v2.md`.

- **D21 Content-addressed resources.** Every IR resource carries `digest`, the SHA-256 of its canonical JSON with backend ids excluded and prompts, env, argv, cwd, readiness and children included. Ownership tokens become `drove_name`, `drove_profile`, `drove_digest`. Drift is a token comparison first; process inspection is a fallback for exited commands. This supersedes the process-info-first wording of D17. Digests never include the repository path.
- **D22 Restart in place.** A changed digest on a `pane(serve=)` restarts the command in the existing pane. Only topology changes (split shape, tab membership) replace a pane, and those stay destructive and confirmed.
- **D23 Readiness probes are host-side** except `output()`. Backends declare `readiness_output`; Radiator sets it false and the planner marks such probes `unsupported`.
- **D24 No caller.** Outside a managed pane the `adopt="caller"` pane is created normally and the journal records `adopted = false`.
- **D25 Prompts** are inline strings or `file("repo/relative/path")`, resolved at compile time into the IR.
- **D26 Lint.** A later `drove lint` checks tasks without `check`, `after` cycles, and unreachable profiles. PR 6 adds a Drovefile corpus that runs against a real Herdr session.

PR 1 gains: `digest()` on every IR resource with a test that key order and backend ids do not change it and any declared field does.
