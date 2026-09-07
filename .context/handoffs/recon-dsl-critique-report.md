# Drovefile DSL critique — recon report

Sources: `docs/spec.md`, `docs/drovefile.md`, `docs/migration.md`,
`src/dsl.rs`, `src/model.rs`, `src/planner.rs`, `examples/basic/Drovefile`,
`~/Development/drove-log-workspace-demo/Drovefile`,
`~/.claude/skills/setup-log-driven-workspace/{SKILL.md,scripts/layout.sh}`.

## 1. Pain points (demo Drovefile excerpts)

**id/label duplication.** `workspace(id="control", label="control", ...)`,
`tab(id="coordinator", label="coordinator", ...)`. Of 3 workspaces/6 tabs/9
panes in the demo, only 3 tabs need a label distinct from id (the model
name embeds in the reactor labels). Everything else is typed twice.

**Binary `split(first, second, ratio)` for what are lists.** `docs/drovefile.md`'s
own 3-pane example nests: `split(direction="right", ratio=0.67, first=pane("editor"), second=split(direction="down", ratio=0.5, first=pane("tests"), second=pane("server")))`.
The two ratios don't read as "3 columns"; reordering panes means
restructuring the tree, not reordering a list.

**Agents declared apart from their pane.** `agent(id="review", pane="review-pane", kind="cursor", args=[...])`
lives in `profile(agents=[...])`, separate from `profile(workspaces=[...])`.
The demo has *no* agent declarations at all, even though its whole point
(per its README) is approximating a layout where two maintenance tabs run
real committer/doc-worker agents — the DSL can't express that without a
disconnected cross-reference by string id.

**Commands are argv arrays only, no readiness.** `layout.sh` does
`herdr pane wait-output "$R_PANE" --match "watching" --timeout 20000` before
moving on. No Drovefile field says "don't consider this pane converged
until it prints X."

**No notion of the invoking pane.** `docs/migration.md`: "The initial Drove
release does not adopt the pane from which it is invoked... the current
controller pane remains unmanaged." But `layout.sh` is built entirely
around adopting the invoking pane ("That pane is not renamed or moved: it
becomes the top of `control › coordinator`"). There's no primitive for
"this declared pane IS wherever I'm standing" — the one thing the real
workflow depends on most is unrepresentable.

**No readiness/dependency/ordering between panes.** `bootstrap` tasks have
`depends_on` (DAG-checked in `src/model.rs`); panes/tabs/agents do not.
`layout.sh` starts commit-reactor before doc-sync and agents only after
their pane exists — ordering lives only in the imperative script.

**No reusable presets beyond `load()`.** `docs/drovefile.md` shows sharing a
fully-built `workspace` value via `load()`, not a parameterized template.
`layout.sh`'s two near-identical reactor tabs (differ only in label, slug,
script arg) require two full hand-written `tab(...pane(...))` blocks;
Starlark `def` helpers work but are undemonstrated anywhere in docs/examples.

**Profiles as whole-workspace lists.** `profile("default", workspaces=[control,maintenance,files])`
vs `profile("core", workspaces=[control,maintenance])` — works only because
workspace-granularity is what differs. No way to say "default minus the
monitor tab" without re-listing or hand-sharing variables; no diff/overlay.

**`profile()` is a side effect, not a value.** `_emit_profile` in
`src/dsl.rs` makes `profile(...)` return `NoneType` while looking like every
other constructor (`pane`, `tab`, `workspace`). `examples/basic/Drovefile`
writes `profiles = [profile(name="default", ...)]` — a list of `None`,
dead code that happens to work. Misleads a reader skimming the file.

**Out of scope, not hidden:** `docs/spec.md` already flags cross-repo/
multi-worktree workspace definitions as future work.

## 2. Hidden assumptions in model/planner

- **Ownership keys on logical id, not content.** Renaming a pane's `id`
  (not `label`) makes Drove treat it as a new pane — old one orphaned/
  detached, new one created — even if the running process is unchanged.
- **Tab replacement destroys live PTYs.** `planner.rs`'s `ReplaceTab` is
  `destructive: true` whenever `cwd`/`command`/`env`/split shape differ; no
  partial reconciliation (e.g. restart one pane's command in place).
- **Layout drift = normalized-JSON equality**, not semantics.
  `layouts_match`/`normalize_layout` strip only `pane_id`/`workspace_id`/
  `tab_id`/`focused`. Any other field Herdr's export adds in the future
  reads as drift → destructive `ReplaceTab` for nothing the user declared.
- **Agents matched by `kind` only**, not args/name (`planner.rs`):
  `actual.pane_id == pane_id && actual.agent == agent.kind`. Changing
  `agent(args=[...])` is invisible to `drove status` as long as the same
  kind is already running.
- **Detach, never delete**, combined with id-as-identity above: renaming an
  id is effectively "abandon the old thing, create a new one" spelled as
  two unlinked actions (`Detach` + `Create...`) instead of an obvious rename.
- **Bootstrap approval keys on `repo_root` path text.**
  `BootstrapTask::digest` hashes `repo_root.to_string_lossy()`; moving the
  checkout directory (no re-clone) invalidates every approval.
- **No limit on split nesting depth** — the model's only way past 2 panes
  in a tab is arbitrarily deep nesting, silently.

## 3. Alternative syntaxes per pain point

Two options each, demo excerpt rewritten under each, Starlark kept throughout.

**id/label.** A — label *is* the id; `id=` only as rare override:
`workspace("control", tabs=[...])`, `tab("commit reactor", id="commit-reactor", ...)`.
B — keep both fields, `label` defaults to `id`: `workspace(id="control", tabs=[...])`
implies `label="control"`.

**Splits → lists.** A — panes list + `split` direction + optional `ratios`
(`len(panes)-1` entries): `tab("main", panes=[pane("editor"), pane("tests"), pane("server", cwd="server", command=["cargo","run"])], split="right", ratios=[0.67, 0.5])`.
Reordering panes is a list-item swap. B — sugar helpers over the existing
tree, no model change: `tab("main", layout=row("editor", column("tests","server")))`.

**Agent placement.** A — nest the agent in its pane, id implicit:
`pane("review-pane", agent=agent(kind="cursor", args=[...]))`.
B — keep agents top-level but require the pane to declare
`expects_agent="cursor"`, so `Profile::validate` catches a mismatched
`pane=` reference at compile time.

**Commands/readiness.** A — a `ready` string field, matched like
`wait-output --match`: `pane("commit-reactor", command=[...], ready="watching")`.
B — a typed readiness value so it can grow: `ready=output_contains("watching")`,
room for `port_open(...)`/`exit_zero(...)` later without breaking the field.

**Invoking pane.** A — a reserved per-pane value: `pane("controller", adopt="invoker")`;
at most one per profile, planner never creates/replaces it, only
reconciles label. B — a profile-level pointer instead of a pane flag:
`profile(name="default", controller_pane="controller", workspaces=[...])`.

**Presets.** A — plain Starlark `def` in a shared `.star` file, no new
primitive: `def reactor_pane(slug, script, model): return pane(slug, command=[...], ready="watching")`,
then `load("drove/presets.star", "reactor_pane")`. B — a first-class
`preset()` wrapper that documents intent and validates required args at
load time: `commit_reactor = preset(reactor_pane, script=None)`. Given
Starlark already supports `def`, A costs nothing today and should simply
be demonstrated in the docs.

**Profiles.** A — flat resource list, Tilt-like, satisfies "flat list of
named resources with placement hints": workspaces/tabs become inferred
groupings from `resource(name, workspace=, tab=, beside=, direction=,
ratio=)` fields rather than separately declared nodes; profiles select by
resource name (`profile("core", resources=["controller","eventlog",...])`),
so dropping one thing is a one-line list diff. Cost: no single block shows
"this workspace has these tabs in this order" — it's reconstructed from
scattered fields. B — keep explicit topology, add overlay:
`default = profile("default", workspaces=[control, maintenance, files])`
then `profile("core", extends=default, drop_workspaces=["files"])`, with
`extends`/`drop_*` validated against what the base actually declares.

## 4. Rewritten demo Drovefile (preferred option, in full)

Preferred: keep explicit topology, but adopt label-defaults-to-id,
panes-as-list with `split`+`ratios`, agent nested in its pane, an `adopt`
pane, a `ready` field, a plain-Starlark preset for the reactor tabs, and
profile `extends`/`drop_workspaces`.

```python
def reactor_tab(label, slug, script, model):
    return tab(
        "{}: {}".format(model, label),
        panes = [pane(slug, command = ["./.context/bin/run-reactor.sh"] + ([script] if script else []), ready = "watching")],
    )

control = workspace("control", tabs = [
    tab("coordinator", panes = [
        pane("controller", adopt = "invoker"),
        pane("eventlog", command = ["./scripts/eventlog-view"]),
    ], split = "down", ratios = [0.5]),
    tab("monitor: system + agents", panes = [pane("agentmon", command = ["./scripts/system-monitor"])]),
])

maintenance = workspace("maintenance", tabs = [
    tab("lazygit", panes = [pane("gitlog", command = ["./scripts/lazygit-or-shell"])]),
    reactor_tab("commit reactor", "commit-reactor", "commit-reactor watches result events and commits declared paths", model = "composer-2.5-fast"),
    reactor_tab("doc sync", "doc-sync", "doc-sync watches committer acknowledgements and updates docs", model = "sonnet"),
])

files = workspace("files", tabs = [tab("files", panes = [pane("spiceedit", command = ["./scripts/files-or-shell"])])])

default = profile("default", workspaces = [control, maintenance, files])
profile("core", extends = default, drop_workspaces = ["files"])
```

22 lines versus the current demo's 96. Biggest contributors: the
`reactor_tab` preset collapsing two near-duplicate 15-line blocks into
2-argument calls; `label` defaulting to `id` removing ~9 duplicated
strings; `extends`/`drop_workspaces` removing the need to re-list
`control`/`maintenance` for `core`.

## 5. What the current design gets right — should survive

- **Constrained Starlark**: no network/clock/filesystem/exec during
  evaluation (`src/dsl.rs`) is the right foundation for stable, diffable,
  replayable desired state — keep it, don't loosen it for ergonomics.
- **Repository-relative `load()` with escape/cycle detection**
  (`compile_module` in `src/dsl.rs`) — the safety base any preset story
  should lean on harder, not replace.
- **Ownership scoped to the selected profile**, not exact-layout
  enforcement (`unmanaged_resources_do_not_create_actions` test) — extra
  user panes must stay "not drift" under any new syntax.
- **Detach-not-delete** for removed declarations — a future flat-resource
  model (§3, profiles option A) must preserve this: removing a `resource()`
  line should only detach, never close, the live thing.
- **Bootstrap approval gated on full source + inputs, remembered until
  changed** (`BootstrapTask::digest`) — a genuinely careful trust boundary;
  don't simplify it away for DSL convenience.
- **Canonical JSON digesting for drift** (`canonical_digest`, BTreeMap-based,
  order-independent) — extend cleanly to whatever new node shapes a rework
  introduces.
