# Radiator hub backend

`src/backend/radiator.rs` implements the `Backend` trait (`src/backend/mod.rs`)
against `radiator-cli`'s hub daemon: an NDJSON RPC protocol over a Unix
domain socket, documented in `radiator-cli/docs/HUB-PROTOCOL.md`. This is the
second Drove backend (spec §7 PR 5); Herdr (`src/backend/herdr.rs`) is the
first.

## What the hub actually gives us

Radiator's hub has no tab or split layer — a workspace is a named group of
flat panes (`crates/proto/src/types.rs` in `radiator-cli`) — no
`agent.start`, and no metadata storage today. `capabilities()` reports this
honestly rather than pretending:

| Capability | Value | Why |
|---|---|---|
| `tabs` | `false` | Hub has no tab concept; a Drove tab flattens into the workspace's pane list. |
| `splits_and_ratios` | `false` | Hub has no pane tree; `split_pane`/`set_ratio` return errors. |
| `workspace_env` | `false` | `workspace.open` takes only `name`. |
| `pane_command_at_create` | `true` | `pane.open` accepts `command`/`args`/`cwd`/`env`. |
| `agent_start` | `false` | No `agent.start`; `start_agent` returns an error pointing at `serve`. |
| `agent_prompt` | `true` | Implemented as `pane.send_text` + an `enter` key. |
| `adopt_caller` | `true` | `RADIATOR_PANE_ID`, injected into every term pane (`HUB-PROTOCOL.md`). |
| `metadata_tokens` | `false` | No `pane.set_metadata` yet; see the journal below. |
| `process_info` | `false` | Snapshot carries no process field yet; see below. |
| `events` | `true` | `events.subscribe` exists; this backend doesn't stream it (PR 7). |

## Flattening tabs

`create_tab` walks the IR's `root` layout tree (the same
`{"type": "split"|"pane", "first", "second", ...}` shape
`ExportedLayout::pane_ids_preorder` already reads for Herdr, plus a flat
`panes` list) and opens one hub pane per leaf, in order, ignoring the split
geometry. A tab with more than one pane, or with a real split node, prints a
`warning:` line naming the flattening (spec D4). Every pane in a workspace —
regardless of which Drove tab it came from — lands under one synthetic tab id
(`{workspace_id}:panes`) in `snapshot()`, since the hub has nowhere else to
put it.

## Ownership tokens: journal, not just the hub

`report_tokens` tries `pane.set_metadata` first. On a hub that doesn't have
it (`error.code == "unknown_method"`), tokens go into a local journal file
instead, keyed by hub pane id, under
`$DROVE_STATE_HOME/radiator-journal/<sha256(socket path)>.json` (falling back
through `$XDG_STATE_HOME`/`~/.local/state/drove` like `src/state.rs`'s own
state file). `resolve_ownership` treats the hub as authoritative the moment
it reports anything at all (via the proposed `PaneInfo.metadata` field): a
hub answer wins outright, and any journal entry for that pane is cleared
rather than compared against it, since a journal entry can only predate a
hub gaining `pane.set_metadata` — it is never a live second writer once the
hub can report metadata itself. `report_tokens` clears the same way on a
successful hub write. `Ownership::Unknown` is reserved for a pane the hub
says nothing about and the journal has never seen either — genuine "we
don't know," not a stale-versus-live mismatch (spec §9, brief item 3).

## Detecting the gap-report hub additions at runtime

`.context/handoffs/recon-radiator-gaps-report.md` proposed five hub
additions: `pane.set_metadata`, `PaneInfo.metadata`, `PaneInfo.process`,
`pane.tail`, `workspace.rename`, `hub.capabilities`. They've since landed on
`radiator-cli` `main` (its PR 21), but this backend still degrades against a
hub that lacks them — an older deployed hub, or a rolling upgrade — rather
than assuming the version it happened to be tested against. Every call
against one of these goes through `RadiatorClient::request_optional`, which
turns the hub's `{"error": {"code": "unknown_method"}}` into `Ok(None)`
instead of a failure:

- `report_tokens` — falls back to the journal (above) only when the hub
  rejects `pane.set_metadata`; on a hub with it, tokens go straight through
  and the journal is never touched.
- `rename_workspace` — prints a `warning:` line and leaves the hub-assigned
  name in place only when the hub rejects `workspace.rename`, rather than
  failing `drove up`.
- `process_info` — reads a `process` field out of the raw `hub.snapshot`
  response if present (the shape `radiator-cli` actually ships:
  `{pid, argv, status, exit_code}`), `None` otherwise; never errors on its
  absence.

`Capabilities::metadata_tokens`/`process_info` still declare `false`
unconditionally (above): they're this backend's static, hub-version-agnostic
contract, not a live probe, so the planner never assumes a specific hub
build. `tests/radiator_smoke.rs` (below) exercises the real, non-degraded
path against the current hub build directly, independent of what
`capabilities()` reports.

## Not implemented here

- `readiness_output` (`ready = output("...")`, D23): the hub's `pane.read`
  returns the visible screen grid only, no scrollback (gaps report item 2:
  `pane.tail` doesn't exist yet). The planner (PR 2) is responsible for
  marking such probes unsupported on this backend; this file doesn't
  advertise a capability for it because `Capabilities` (frozen by PR 1,
  `src/backend/mod.rs`) has no such field yet.
- CLI wiring for `--backend radiator`. `selected_by_environment()` in this
  file implements the environment-variable half of brief item 4 (default to
  Radiator when `RADIATOR_HUB_SOCKET` is set and `HERDR_ENV` is not), but
  `src/cli.rs` isn't in this PR's claimed paths (spec §7, PR 5 row) — wiring
  the flag and the fallback order is left for whichever PR touches the CLI.
- Real hub changes. Everything above is client-side detection and
  degradation; the hub additions themselves are `radiator-cli`'s to make, not
  this repo's.

## Testing

`src/backend/radiator.rs`'s `#[cfg(test)]` module covers socket discovery,
the NDJSON request/response envelope (including truncated responses and
`unknown_method` degradation), pane-spec extraction from a layout tree,
snapshot flattening, and the ownership journal's agree/disagree cases — all
against a fake hub over a real Unix socket, no `radiator-cli` checkout
required.

`tests/radiator_smoke.rs` is a real end-to-end check against an actual hub
daemon: open a workspace, open a pane, read it back from `hub.snapshot`,
report tokens and confirm the hub itself now reports them back
(`resolve_ownership`), read `process_info` back for the spawned pane, rename
the workspace and confirm the hub applied it, then close both. It's skipped
(not failed) unless `RADIATOR_SMOKE_SOCKET` is set.

`scripts/smoke-radiator.sh` builds `radiator-cli` (from `../radiator-cli`
next to this repo, or `$RADIATOR_CLI_ROOT`), starts its hub, and runs the
test above against it — or prints why it skipped and exits 0 if
`radiator-cli` isn't checked out or won't build, so CI without that sibling
repo still passes.

**Isolation.** `radiator hub`'s persisted-layout state file is keyed by
`--hub-name` alone (`radiator_hub::paths::state_path`), independent of
`--socket` — passing `--socket` without also pinning `--hub-name` still
persists to the *default* hub's state file (`hub-main.layout.json`), because
`--hub-name` silently defaults to `"main"`. An earlier version of this script
did exactly that, and its throwaway `drove-smoke` workspace leaked into a
live `main` hub's persisted layout. The script now runs the hub with
`XDG_RUNTIME_DIR` pointed at its own scratch directory and a unique
`--hub-name`, so both its socket and its state file are fully isolated —
mirroring `scripts/smoke-herdr.sh`'s unique `--session` for the same
reason. Verify this holds before trusting a change here: diff
`~/.local/state/radiator/hub-main.layout.json` (or wherever a real hub on
the machine persists to) before and after a run.
