# Drove

Drove is a versioned workspace reconciler for [Herdr](https://herdr.dev): a checked-in `Drovefile` describes the workspaces, tabs, panes, agents, and bootstrap tasks a repository needs.

It is intentionally on demand. Drove reports drift and changes Herdr only when you run it.

## Install

Requirements:

- Rust 1.88 or newer
- Herdr 0.8.2 or newer

From this checkout:

```sh
cargo install --path .
```

## Quick start

Add a `Drovefile` to a repository:

```python
profile(
    name = "default",
    workspaces = [
        workspace(
            id = "development",
            label = "development",
            tabs = [
                tab(
                    id = "main",
                    label = "editor + tests",
                    layout = split(
                        direction = "right",
                        ratio = 0.67,
                        first = pane(id = "editor", label = "editor"),
                        second = pane(id = "tests", label = "tests"),
                    ),
                ),
            ],
        ),
    ],
)
```

Then run:

```sh
drove plan
drove up
drove status
```

`drove` with no subcommand is equivalent to `drove up`. Use `--profile review` for a named profile and `--session NAME` for a named Herdr session.

## Safety model

- Drove mutates only resources recorded in its machine-local ownership state.
- Extra Herdr resources are ignored and preserved.
- Removing a declaration detaches it; Drove does not delete the live resource.
- Topology drift may require replacing a managed tab. Replacement discards its PTYs, scrollback, and running processes, so it requires `--allow-replace`.
- New or changed bootstrap task bytes require approval before either their check or run command executes. Review them, then use `drove up --yes` or approve interactively.
- Runtime IDs and approvals are stored outside the repository.

Pane `command` values are for long-running processes. Use a bootstrap task for a one-shot command; when a pane command exits, Herdr removes that pane and Drove correctly reports layout drift.

## Exit status

- `0`: in sync or successfully reconciled
- `1`: invalid configuration or apply failure
- `2`: valid profile is out of sync
- `3`: Herdr is not running or its socket cannot be reached

See [the Drovefile reference](docs/drovefile.md), [migration guide](docs/migration.md), and [product spec](docs/spec.md).
