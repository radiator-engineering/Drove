# Migrating a log-driven Herdr workspace

Drove replaces the imperative layout portion of `/setup-log-driven-workspace`; it does not replace the event log, reactor contracts, or repository standing orders.

## 1. Commit durable project files

Run the existing scaffold once before migration and commit its tracked output:

- `AGENTS.md` and `CLAUDE.md`
- `.claude/settings.json`
- `.context/DECISIONS.md`
- reactor scripts and briefs
- repository-owned setup/check scripts

Do not make Drove rewrite these tracked files on every reconciliation. They are ordinary source-controlled project content.

## 2. Keep local activation explicit

Represent machine-local hooks or configuration as bootstrap tasks only when they have:

- an idempotent `check` command
- an idempotent `run` command
- every repository script listed in `inputs`

Changed checks and runs are approval-gated. This is the equivalent of the setup workflow's deliberate trust boundary.

## 3. Translate the layout

Map each imperative surface to a logical Drove resource:

| Existing surface | Drove declaration |
| --- | --- |
| `control` workspace | `workspace(id = "control", ...)` |
| coordinator/event-log split | one `tab` with a `split(direction = "down", ...)` layout |
| maintenance workspace | a second `workspace` |
| commit and docs reactors | panes with long-running `command` argv |
| monitor and files workspaces | named profiles or optional workspaces |
| agent launch after layout | `agent(...)` referencing a logical pane ID |

The initial Drove release does not adopt the pane from which it is invoked. It creates and owns declared resources, while the current controller pane remains unmanaged and preserved.

## 4. Preserve reactor behavior

Drove creates the panes; the reactor scripts still own:

- event matching and acknowledgement
- lock acquisition
- retries and crash-loop handling
- append-only log rules
- clean-tree and staging boundaries

Do not turn reactors into bootstrap tasks. Bootstrap tasks terminate; reactors are long-running pane commands.

## 5. Cut over safely

1. Stop the old layout reactors through their existing teardown command.
2. Run `drove plan` and verify every create action.
3. Run `drove up`.
4. Run the existing workspace health command.
5. Append a small real result event and verify the commit and documentation acknowledgement loop.
6. Run `drove up` again; it must report `in sync`.

Extra tabs or panes created manually are intentionally ignored. Removing a declaration detaches its live resource rather than closing it.
