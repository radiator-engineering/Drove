# Migrating a log-driven Herdr workspace

Drove owns the terminal layout. The `eventlog` CLI owns log validation,
reactor lifecycles, locks, resume, action timeouts and acknowledgments.
Keep the log and existing work when replacing an older shell-based setup.

## Install and configure eventlog

For the native log-driven reactor layout, install eventlog 0.2.0 or newer;
it supplies the `setup`, `action` and `lifecycle` support this layout uses.
The public upstream release is
https://github.com/radiator-engineering/eventlog/releases/tag/v0.2.0. From
the repository root, run:

```sh
eventlog setup preview
eventlog setup apply
eventlog doctor --fix
```

Setup preserves the existing log and Drovefile. Configure identities, models,
timeouts and documentation roots in `.context/eventlog-setup.toml`, then run
`eventlog setup preview` and `eventlog setup upgrade` to regenerate
`.context/eventlog-reactors.star`. Keep that generated file intact; customized
helper content causes a conflict instead of being overwritten. Repeating
setup after an unchanged configuration reports no changes.

Model names alone do not invoke a model. This repository uses
`[commit].command = ["python3", ".context/bin/model-command.py", "commit"]`
for Composer 2.5 Fast and the corresponding `docs` command for Claude Sonnet.
The small Python command supplies the project briefs and returns the model's
exit status. It requires Python 3.11 or newer, `cursor-agent` and `claude`,
with their normal login credentials. The docs command uses the Claude login
unless `DOC_USE_API_KEY=1` explicitly selects the environment's API key.
Configured commit commands for this native reactor layout require eventlog
0.2.0 or newer; the older direct-Git action does not preserve Composer
behavior.

## Declare the layout

Load the generated helper and add its tabs to the maintenance workspace:

```python
load(".context/eventlog-reactors.star", "eventlog_reactors")

backend("herdr")
herdr.session("my-project")
maintenance = workspace("maintenance", panes = eventlog_reactors())
profile("default", workspaces = [maintenance])
```

The helper runs native reactors and calls `eventlog lifecycle start/stop`
through Drove hooks. Use `eventlog view --follow` in the log-viewer pane.
The repository Drovefile and `examples/log-driven` show a complete layout.
The example includes generated assets and model bindings; configure its
policy for the consuming repository before activation.

## Recover an existing log before activation

First inspect `eventlog state`, the historical acknowledgments, the actual
Git history, and the dirty index and working tree. Stop old reactors through
their existing lifecycle, with user authorization. Never delete log history
or reactor locks to reset the workspace.

If a commit succeeded without an acknowledgment, recover that acknowledgment
only from verified commit evidence. Preserve its real commit refs so the doc
worker receives the missing trigger. Record pending work as new results with
exact paths before superseding old broad scopes. Never advance a checkpoint
past work that has no replacement result or verified disposition.

Probe recovery on a disposable copy first. `eventlog react test` executes its
action; its runtime output is a dry run, but the action can still commit,
edit files or append documentation results. Test model commands with stubs
before using the real model credentials.

## Apply and verify

Inspect `drove plan` before `drove up --no-focus --yes`. Manually created
workspaces are initially unmanaged: a create plan does not adopt them by
matching labels. Preserve the controller pane and verify any one-time local
ownership recovery against the live session before applying.

Verify a result → commit ack → docs result → commit ack chain, with exact
committed paths and no repeated docs invocation. Confirm that unrelated
staged files remain staged. Then repeat `eventlog setup upgrade` and
`drove up --no-focus --yes`; the unchanged setup and layout should converge.
See `.context/reports/eventlog-cutover.md` for this repository's historical
recovery evidence.
