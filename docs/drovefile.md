# Drovefile reference (v3)

`Drovefile` is deterministic Starlark evaluated from the repository root. It can use ordinary Starlark expressions and repository-local `load()` statements, but Drove exposes no network, clock, environment, filesystem, or command-execution functions during evaluation.

Core constructors — `backend`, `workspace`, `pane`, `caller_pane`, `agent`, `task`, `profile` — stay bare. Backend-specific terminology lives under a namespace object: `herdr` for Herdr's flavor, `radiator` for the Radiator hub's. A Drovefile that uses only the core reconciles on every backend with no `unsupported` outcomes; using a flavor construct on a backend that doesn't implement it produces an `unsupported` outcome instead of silently dropping it.

Four resource kinds — `workspace`, `pane`, `agent`, `task` — share one profile-scoped namespace for `after`, `on_start`/`on_stop`, and adoption references. Names must match `[a-z][a-z0-9_-]{0,31}`, except `herdr.tab` names, which are free-form placement labels (a tab is a Herdr display hint, not part of the shared reference namespace).

## `backend` and target instance

```python
backend("herdr")          # core: which backend this project reconciles onto
herdr.session("drove")    # Herdr flavor: which named session
radiator.hub("main")      # Radiator flavor: which named hub
```

A Drovefile may declare both flavor instances; only the active backend's declaration is used. The backend id and target instance resolve in this order, most specific first:

1. CLI: `--backend <id>`, `--target <name>` (`--session` is a Herdr alias of `--target`; `--socket` is an explicit override).
2. Environment: `DROVE_BACKEND`; `HERDR_SESSION` or `RADIATOR_HUB` per backend; `HERDR_SOCKET_PATH` as before.
3. Drovefile: `backend(...)`, `herdr.session(...)`, `radiator.hub(...)`.
4. Built-in: backend `herdr`; Herdr session `default`; Radiator hub `main`.

A Drovefile's declaration is a default that an explicit flag or an ambient session overrides.

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

`extends` and `without` accept either a resource value or its name string; the value form is the documented one:

```python
profile("default", workspaces = [control, maintenance, files])
profile("core", extends = default, without = [files])
```

`extends` must reference an already-declared profile. `profile()` returns the value it registers. Composition is resolved after the whole file evaluates — the sandbox never touches other profiles or the filesystem during Starlark evaluation itself.

## Workspaces, panes, and groups

```python
control = workspace("control", panes = [
    herdr.tab("coordinator", split = herdr.DOWN, ratios = [0.5], panes = [
        caller_pane("controller"),
        pane("eventlog", serve = ["eventlog-view.sh", "-f"]),
    ]),
    herdr.tab("monitor", panes = [pane("agentmon", serve = ["htop"])]),
])
```

`workspace(name, panes = [...])` is the core signature; `label`, `cwd`, `env`, and `was` (below) are also core. The `panes` list accepts bare `pane(...)` values and **groups**. A group is a flavor value that carries its own panes plus a placement; the compiler flattens each group into the core pane list and writes the placement onto every pane it contains. A bare pane in the list — one not wrapped in a group — carries no placement.

`herdr.tab(name, panes, split = herdr.RIGHT, ratios = [])` is the first group. `herdr.RIGHT` and `herdr.DOWN` are the split constants. A tab lists its panes and a split direction; there is no binary split tree on the surface. A backend without tabs and splits flattens the layout and reports `unsupported` rather than dropping it silently.

## Panes

```python
pane(
    name = "server",
    label = None,           # defaults to `name`
    cwd = "server",
    env = {"RUST_LOG": "info"},
    serve = ["cargo", "run"],
    ready = None,            # output("text"), port(n), or cmd([...])
    after = [],               # panes/tasks that must be ready first
    agent = None,
    on_start = None,
    on_stop = None,
    was = None,
)
```

`serve` is the long-running process; a pane without `serve` is a plain terminal. `serve = any_of([argv1], [argv2])` tries each candidate argv in order and records the first whose executable is on `PATH`.

`caller_pane(name, ...)` declares the pane Drove never creates, moves, or replaces — the invoking terminal. It takes the same fields as `pane` except `adopt`. At most one `caller_pane` per profile.

Readiness gates `after`: `output("watching")` matches pane output, `port(8080)` probes a TCP port, `cmd(["curl", "-f", "..."])` runs a command. The reconciler is planned to re-check readiness on every reconcile, not just at start; this PR only compiles readiness into the IR.

`on_start` and `on_stop` are argv hooks Drove runs once per actual start or stop, in the repository root, with `DROVE_RESOURCE` and (when known) `DROVE_BACKEND_ID` in the environment. `task()` hooks run around `run`: `on_start` fires once `run` has executed, whether or not it succeeded. Pane hooks fire once pane reconciliation against a live backend exists; today `on_start` on a pane is parsed and carried into the IR but not yet run, and `on_stop` on a pane runs only from `drove down`. Every hook argv is approval-gated the same way a task's `run` is (`drove run --yes` / `drove up --yes` / `drove down --yes` to approve on the spot).

## Renaming without loss: `was`

A pane or workspace may declare `was = "old-name"`:

```python
pane(name = "review", was = "shell")
workspace(name = "control", was = "coordination")
```

When the backend holds a live resource whose ownership token equals `old-name` and no live resource is named `new-name`, the plan renames it in place instead of detaching the old resource and creating a new one — the same identity, carried forward, with no `Detach`. After one successful apply the declaration is inert; a `was` that matches nothing live is a `drove lint` warning, not an error.

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

## Migration shims

Every v2 form below still compiles for one release. Each one warns with the exact v3 rewrite, and `drove render` prints the file back in v3 form:

| v2 form | v3 rewrite | Warns |
| --- | --- | --- |
| `pane(name, adopt = "caller")` | `caller_pane(name, ...)` | `pane("name", adopt = "caller") is a v2 form; rewrite as caller_pane("name", ...)` |
| `tab(name, ...)` | `herdr.tab(name, ...)` | `tab("name") is a v2 form; rewrite as herdr.tab("name", ...)` |
| `workspace(name, tabs = [...])` | `workspace(name, panes = [herdr.tab(...)])` | `workspace("name", tabs = [...]) is a v2 form; rewrite as workspace("name", panes = [herdr.tab(...)])` |
| `split = "right"` / `"down"` | `split = herdr.RIGHT` / `herdr.DOWN` | `split = "right" is a v2 form; rewrite as split = herdr.RIGHT` (and the `down` equivalent) |

See `docs/upgrading-v3.md` for the full v2 → v3 migration.

## Commands

```sh
drove status [--profile NAME] [--json]
drove plan   [--profile NAME] [--json]
drove up     [--profile NAME] [--yes] [--allow-replace]
drove render [--profile NAME] [--json]
drove run    [NAME] [--yes]
drove down   [--profile NAME] [--purge] [--yes]
drove lint   [--profile NAME] [--json]
```

`drove render` prints the compiled intermediate representation (IR schema version 3): a flat, deterministically ordered list of typed resources, each carrying a content digest, plus a topology digest per placement group (`was` renames and moving a pane between tabs change the topology digest, never the content one). Given a v2 Drovefile, it also prints every deprecation warning and the file's v3 form. It performs no backend I/O.

`drove lint` warns on a `was` that matches nothing live and on a task with no `check`; it always exits `0`.

`drove up` also runs every `auto = True` task the plan proposes (`RunTask` actions), in `after` order; reconciling workspaces, panes and agents against a live backend is a later PR. `drove run NAME` runs one task and its `after` prerequisites, and nothing else declared in the profile; with no `NAME`, it lists every declared task and its last recorded outcome. `drove down` selects the resources local state records as owned by this profile, runs each one's `on_stop` hook, then stops tracking it (`--purge` also passes each resource's stored backend id to the backend's `close_pane`); it never touches a pane local state doesn't record as owned by this profile.

Use `--backend ID`, `--target NAME`, `--file PATH`, `--socket PATH`, or `--session NAME` when discovery defaults are not appropriate.
