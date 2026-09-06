# Drovefile reference (schema version 2)

`Drovefile` is deterministic Starlark evaluated from the repository root. It can use ordinary Starlark expressions and repository-local `load()` statements, but Drove exposes no network, clock, environment, filesystem, or command-execution functions during evaluation.

Every file must declare a `default` profile. Five resource kinds — `workspace`, `tab`, `pane`, `agent`, `task` — share one profile-scoped namespace for `after`, `on_start`/`on_stop`, and adoption references. Names must match `[a-z][a-z0-9_-]{0,31}`, except tab names, which are free-form placement labels (tabs are a Herdr display hint, not part of the shared reference namespace).

## `profile`

```python
profile(
    name = "default",
    workspaces = [],
    tasks = [],
    extends = None,
    without = [],
)
```

`extends` composes profiles by name; `without` removes named workspaces from the composed list:

```python
profile("default", workspaces = [control, maintenance, files])
profile("core", extends = "default", without = ["files"])
```

`extends` must reference an already-declared profile. `profile()` returns the value it registers. Composition is resolved after the whole file evaluates — the sandbox never touches other profiles or the filesystem during Starlark evaluation itself.

## Workspaces and tabs

```python
workspace(
    name = "development",
    label = None,      # defaults to `name`
    cwd = ".",
    env = {},
    tabs = [
        tab(
            name = "main",
            label = "editor + tests",
            split = "right",       # "right" or "down"
            ratios = [0.67],       # len(panes) - 1 entries, each in [0.05, 0.95]
            panes = [
                pane(name = "editor"),
                pane(name = "tests"),
            ],
        ),
    ],
)
```

A tab lists its panes and a split direction; there is no binary split tree on the surface. A backend without tabs or splits flattens the layout and warns.

## Panes

```python
pane(
    name = "server",
    label = None,           # defaults to `name`
    cwd = "server",
    env = {"RUST_LOG": "info"},
    serve = ["cargo", "run"],
    ready = None,            # output("text"), port(n), or cmd([...])
    after = [],               # names of panes/tasks that must be ready first
    adopt = None,             # "caller", at most one pane per profile
    agent = None,
    on_start = None,
    on_stop = None,
)
```

`serve` is the long-running process; a pane without `serve` is a plain terminal. `serve = any_of([argv1], [argv2])` tries each candidate argv in order and records the first whose executable is on `PATH`. Exactly one pane per profile may declare `adopt = "caller"`; Drove never creates, moves, or replaces that pane — it is the invoking terminal.

Readiness gates `after`: `output("watching")` matches pane output, `port(8080)` probes a TCP port, `cmd(["curl", "-f", "..."])` runs a command. The reconciler is planned to re-check readiness on every reconcile, not just at start; this PR only compiles readiness into the IR.

`on_start` and `on_stop` are argv hooks Drove runs once per actual start or stop, in the repository root, with `DROVE_RESOURCE` and (when known) `DROVE_BACKEND_ID` in the environment. `task()` hooks run around `run`: `on_start` fires once `run` has executed, whether or not it succeeded. Pane hooks fire once pane reconciliation against a live backend exists; today `on_start` on a pane is parsed and carried into the IR but not yet run, and `on_stop` on a pane runs only from `drove down`. Every hook argv is approval-gated the same way a task's `run` is (`drove run --yes` / `drove up --yes` / `drove down --yes` to approve on the spot).

## Agents

An agent is a property of its pane:

```python
pane(
    name = "review",
    agent = agent(
        kind = "claude",
        args = ["--model", "sonnet"],
        prompt = "Read .context/handoffs/review.md and do only that.",
    ),
)
```

`prompt` is either an inline string (capped at 2 KB) or `file("repo/relative/path")`, resolved into the prompt text at compile time. The available `kind` and argument values are defined by the installed backend.

## Tasks

Tasks are one-shot, convergent setup actions:

```python
task(
    name = "install-hooks",
    run = ["./scripts/install-hooks"],
    check = ["./scripts/install-hooks", "--check"],
    inputs = ["scripts/install-hooks"],
    after = [],
    auto = True,
    on_start = None,
    on_stop = None,
)
```

If `check` succeeds, `drove up` skips `run` (early cutoff). `auto = True` (the default) runs the task during `drove up` once its `after` set is ready; `auto = False` requires an explicit `drove run <name>`. Task dependencies (`after`) form a directed acyclic graph together with pane `after` references, since both live in the same namespace.

Running `run` is approval-gated on the digest of its argv: the first time it needs to run, `drove up`/`drove run` reports it as blocked until re-run with `--yes` (or the same digest is approved again after the task's declared `run` changes).

## Commands

```sh
drove status [--profile NAME] [--json]
drove plan   [--profile NAME] [--json]
drove up     [--profile NAME] [--yes] [--allow-replace]
drove render [--profile NAME] [--json]
drove run    [NAME] [--yes]
drove down   [--profile NAME] [--purge] [--yes]
```

`drove render` prints the compiled intermediate representation (schema version 2): a flat, deterministically ordered list of typed resources, each carrying a content digest. It performs no backend I/O.

`drove up` also runs every `auto = True` task the plan proposes (`RunTask` actions), in `after` order; reconciling workspaces, tabs, panes and agents against a live backend is a later PR. `drove run NAME` runs one task and its `after` prerequisites, and nothing else declared in the profile; with no `NAME`, it lists every declared task and its last recorded outcome. `drove down` runs each owned resource's `on_stop` hook, then stops tracking it (`--purge` also closes owned panes on the backend); it never touches a pane the backend doesn't report as owned by this profile.

Use `--file PATH`, `--socket PATH`, or `--session NAME` when discovery defaults are not appropriate.
