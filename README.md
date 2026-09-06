# Drove

Drove is a versioned workspace reconciler. A checked-in `Drovefile` describes the workspaces, tabs, panes, agents, and setup tasks a repository needs. Drove compiles that file to a canonical model, compares it to the live state of a backend, and reports or applies the difference.

Drove is backend-agnostic. [Herdr](https://herdr.dev) is the backend available today. A second backend for the Radiator hub is in progress, and the model is designed so other backends can follow. Each backend declares what it supports, and Drove plans only the operations that backend can perform.

Drove is intentionally on demand. It reads the live state and changes it only when you run it.

## Install

Requirements:

- Rust 1.88 or newer
- A supported backend. Herdr 0.8.2 or newer is the one available today.

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
            name = "development",
            tabs = [
                tab(
                    name = "main",
                    label = "editor + tests",
                    split = "right",
                    ratios = [0.67],
                    panes = [
                        pane(name = "editor"),
                        pane(name = "tests"),
                    ],
                ),
            ],
        ),
    ],
)
```

A pane can run a command, host a coding agent, and gate its readiness on output or a port. A `task` runs a one-shot setup step with a `check` that lets Drove skip it once it has run. See the [Drovefile reference](docs/drovefile.md) for every resource and field, and `examples/log-driven/Drovefile` for a full multi-agent workspace.

Then run:

```sh
drove render
drove plan
drove status
drove up
```

`drove render` prints the compiled model as a flat, ordered list of resources, each with a content digest. It does no backend I/O. `drove plan` and `drove status` compare the model to the live backend and report drift. `drove up`, the default command, applies the plan. Reconciliation is being wired in backend by backend; see the reference for the current state of each command.

Use `--profile NAME` for a named profile, `--session NAME` for a named backend session, and `--file PATH` when Drove cannot find the `Drovefile` by searching parent directories.

## Development

Run `make ci` to check formatting, clippy, tests, docs, and the security audit locally. These are the same checks CI runs.

## Safety model

- Drove changes only resources recorded in its machine-local ownership state.
- Drove leaves resources it does not own untouched.
- Removing a declaration detaches the resource. Drove does not delete the live thing.
- A changed pane command restarts in place. Only a topology change replaces a tab, which discards its scrollback and running processes, so it requires `--allow-replace`.
- A new or changed task requires approval before its check or run command executes. Review the bytes, then approve with `drove up --yes` or interactively.
- Drove stores ownership state and approvals outside the repository.

## Exit status

- `0`: in sync, or reconciled successfully
- `1`: invalid configuration or apply failure
- `2`: a valid profile is out of sync
- `3`: the backend is not running or its socket cannot be reached

See the [Drovefile reference](docs/drovefile.md), [migration guide](docs/migration.md), and [product spec](docs/spec.md).
