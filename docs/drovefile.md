# Drovefile reference

`Drovefile` is deterministic Starlark evaluated from the repository root. It can use ordinary Starlark expressions and repository-local `load()` statements, but Drove exposes no network, clock, environment, filesystem, or command-execution functions during evaluation.

Every file must declare a `default` profile. Logical IDs are stable ownership addresses and must be unique in their scope.

## `profile`

```python
profile(
    name = "default",
    workspaces = [],
    agents = [],
    bootstrap = [],
)
```

Additional profiles can share values loaded from `.star` files:

```python
load("drove/common.star", "development_workspace")

profile(name = "default", workspaces = [development_workspace])
profile(name = "review", workspaces = [development_workspace])
```

Load paths are repository-root-relative. Absolute paths, missing files, escaping symlinks, duplicate IDs, missing references, and dependency cycles are errors.

## Workspaces and tabs

```python
workspace(
    id = "development",
    label = "development",
    cwd = ".",
    tabs = [
        tab(
            id = "main",
            label = "main",
            layout = pane(id = "shell", label = "shell"),
        ),
    ],
)
```

`cwd` values are repository-relative. The `id` is Drove's stable logical identity; `label` is Herdr presentation and can be reconciled independently.

## Pane layouts

A tab has one pane or a binary split tree:

```python
split(
    direction = "right",
    ratio = 0.67,
    first = pane(id = "editor", label = "editor"),
    second = split(
        direction = "down",
        ratio = 0.5,
        first = pane(id = "tests", label = "tests"),
        second = pane(
            id = "server",
            label = "server",
            cwd = "server",
            command = ["cargo", "run"],
            env = {"RUST_LOG": "info"},
        ),
    ),
)
```

Directions are `right` and `down`; ratios must be between `0.05` and `0.95`. Commands are argv arrays, not shell strings. They must remain running for the pane to remain part of the desired layout.

## Agents

Agents reference a logical pane ID and start after Herdr returns its runtime pane ID:

```python
agent(
    id = "review",
    pane = "review-pane",
    kind = "cursor",
    name = "review",
    args = ["--model", "composer-2.5-fast"],
)
```

The available `kind` and argument values are defined by the installed Herdr version.

## Bootstrap tasks

Bootstrap tasks are one-shot, convergent setup actions:

```python
bootstrap(
    id = "install-hooks",
    check = ["./scripts/install-hooks", "--check"],
    run = ["./scripts/install-hooks"],
    inputs = ["scripts/install-hooks"],
    depends_on = [],
)
```

Both `check` and `run` are argv arrays. Drove requires approval for the task declaration, the complete loaded Drovefile source, and every declared input before it executes either command. If the approved check succeeds, Drove skips the run command. After running, the check must succeed.

Task dependencies form a directed acyclic graph and run in dependency order.

## Commands

```sh
drove status [--profile NAME] [--json]
drove plan [--profile NAME] [--json]
drove up [--profile NAME] [--yes] [--allow-replace]
```

Use `--file PATH`, `--socket PATH`, or `--session NAME` when discovery defaults are not appropriate.
