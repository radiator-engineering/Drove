# Drove

Drove is a versioned workspace reconciler. A checked-in `Drovefile` describes the workspaces, tabs, panes, agents, and setup tasks a repository needs. Drove compiles that file to a canonical model, compares it to the live state of a backend, and reports or applies the difference.

Drove is backend-agnostic. [Herdr](https://herdr.dev) is the backend available today. A second backend for the Radiator hub is in progress, and the model is designed so other backends can follow. Each backend declares what it supports, and Drove plans only the operations that backend can perform.

Drove is intentionally on demand. It reads the live state and changes it only when you run it.

## Install

`drove` ships prebuilt binaries for macOS (Intel and Apple silicon), Linux
(x86_64 and arm64) and Windows (x86_64) with every release. You also need a
supported backend; Herdr 0.8.2 or newer is the one available today.

Homebrew (macOS and Linux):

```sh
brew install radiator-engineering/tap/drove
```

Shell installer (macOS and Linux):

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/radiator-engineering/Drove/releases/latest/download/drove-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -c "irm https://github.com/radiator-engineering/Drove/releases/latest/download/drove-installer.ps1 | iex"
```

With Cargo (needs Rust 1.88 or newer):

```sh
cargo install drove     # compile from crates.io
cargo binstall drove    # download a prebuilt binary, no compile
```

Or from a checkout of this repository:

```sh
cargo install --path .
```

Every [GitHub release](https://github.com/radiator-engineering/Drove/releases)
also carries the raw binaries and their SHA256 checksums.

## Quick start

Add a `Drovefile` to a repository:

```python
backend("herdr")

profile(
    name = "default",
    workspaces = [
        workspace(
            name = "development",
            panes = [
                herdr.tab(
                    name = "main",
                    label = "editor + tests",
                    split = herdr.RIGHT,
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

`backend(...)` declares which backend the project reconciles onto; `herdr` is the Herdr flavor's namespace for placement (tabs, splits, ratios). A pane can run a command, host a coding agent, and gate its readiness on output or a port. A `task` runs a one-shot setup step with a `check` that lets Drove skip it once it has run. See the [Drovefile reference](docs/drovefile.md) for every resource and field, and `examples/log-driven/Drovefile` for a full multi-agent workspace.

Then run:

```sh
drove render
drove plan
drove status
drove up
```

`drove render` prints the compiled model as a flat, ordered list of resources, each with a content digest. It does no backend I/O. `drove plan` and `drove status` compare the model to the live backend and report drift. `drove up`, the default command, applies the plan. Reconciliation is being wired in backend by backend; see the reference for the current state of each command.

Use `--profile NAME` for a named profile, `--backend ID` / `--target NAME` (or `--session NAME` on Herdr) to override the declared backend and instance, and `--file PATH` when Drove cannot find the `Drovefile` by searching parent directories. Run `drove lint` to catch a stale `was = "..."` rename or a task with no `check`.

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

See the [Drovefile reference](docs/drovefile.md), [migration guide](docs/migration.md), [v2 to v3 upgrade guide](docs/upgrading-v3.md), and [product spec](docs/spec.md).
