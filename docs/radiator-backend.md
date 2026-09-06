# Radiator hub backend

`src/backend/radiator.rs` implements the core `Backend` trait (`src/backend/mod.rs`)
against `radiator-cli`'s hub daemon: an NDJSON RPC protocol over a Unix
domain socket, documented in `radiator-cli/docs/HUB-PROTOCOL.md`. This is the
second Drove backend; Herdr (`src/backend/herdr.rs`) is the first.

`RadiatorClient` implements only the core `Backend` trait, not `HerdrExt`
(D28) — `Backend::herdr()` returns its default `None`. The Radiator flavor
itself is deferred (D37): `radiator` in the prelude exposes only `hub(...)`
today, so a Drovefile has nothing Radiator-specific to declare beyond which
hub instance it targets.

## Core capabilities

`capabilities()` reports the graded core features only (spec §3, D27) — the
flavor verbs Herdr alone implements (tabs, splits, ratios, agent start) are
not in this struct at all, because their absence or presence is the
`herdr()`/`radiator()` accessor, not a bool:

| Capability | Value | Why |
|---|---|---|
| `workspace_env` | `false` | `workspace.open` takes only `name`; no `cwd`/`env` param exists to verify against. |
| `pane_command_at_create` | `true` | `pane.open` accepts `command`/`args`/`cwd`/`env`. |
| `metadata_tokens` | from `hub.capabilities` | See D35 below. |
| `process_info` | from `hub.capabilities` | See D35 below. |
| `events` | `true` | `events.subscribe` exists; this backend doesn't stream it yet. |
| `readiness_output` | `true` | `pane.tail` backs the `output()` readiness probe. |

A workspace with no placement group is a flat list of panes on Radiator: the
hub has no tab or split layer at all (`crates/proto/src/types.rs` in
`radiator-cli`). `snapshot()` still reports one synthetic tab id per
workspace (`{workspace_id}:panes`) purely so the shared `SessionSnapshot`
shape has somewhere to put every pane — that id is an internal reporting
detail, not a placement a Drovefile can address.

**D38 caveat.** A bare `pane(...)` in a workspace's `panes` list is supposed
to carry no placement at all (D31). The v3 DSL compiler currently wraps it in
an implicit single-pane Herdr group instead, so it always carries
`Placement::Herdr { tab: <pane name> }` — even when `backend("radiator")` is
declared. No example or test uses a bare pane today, so this is latent, not
a live bug, but it must be fixed (`ir.rs`/`executor.rs`) before a Drovefile
ships bare panes on a non-Herdr backend.

## Ownership tokens: journal, not just the hub

`report_tokens` tries `pane.set_metadata` first. On a hub that doesn't have
it (`error.code == "unknown_method"`), tokens go into a local journal file
instead, keyed by hub pane id, under
`$DROVE_STATE_HOME/radiator-journal/<sha256(socket path)>.json` (falling back
through `$XDG_STATE_HOME`/`~/.local/state/drove` like `src/state.rs`'s own
state file). `resolve_ownership` treats the hub as authoritative the moment
it reports anything at all: a hub answer wins outright, and any journal
entry for that pane is cleared rather than compared against it, since a
journal entry can only predate a hub gaining `pane.set_metadata` — it is
never a live second writer once the hub can report metadata itself.
`report_tokens` clears the same way on a successful hub write.
`Ownership::Unknown` is reserved for a pane the hub says nothing about and
the journal has never seen either — genuine "we don't know," not a
stale-versus-live mismatch.

## D35: capabilities read from the hub at connect

Hub commit `80c0f1d` (`radiator-cli`) landed `pane.set_metadata`,
`PaneInfo.metadata`, `PaneInfo.process`, `pane.tail`, `workspace.rename` and
`hub.capabilities`. `RadiatorClient::hub_capabilities` queries
`hub.capabilities` once per client and caches the reply for its lifetime,
gating `metadata_tokens` and `process_info`:

- A hub that reports metadata support is authoritative for tokens; the local
  journal is never consulted.
- A hub that doesn't is served from the journal alone.
- An older hub with no `hub.capabilities` at all answers `unknown_method`,
  folded into the same all-`false` default as a hub that explicitly reports
  no optional features.
- A transport failure during the query is reported as all-`false` for that
  call only, without being cached, so a transient blip doesn't permanently
  strand the client on the journal-only path.

`rename_workspace` calls `workspace.rename` and prints a `warning:` line and
leaves the hub-assigned name in place if the hub rejects the call, rather
than failing `drove up`. `process_info` reads a `process` field out of the
raw `hub.snapshot` response if present (`{pid, argv, status, exit_code}`),
`None` otherwise.

## Not implemented here

- CLI wiring beyond `select::open`. `src/backend/select.rs` resolves the
  backend id and target instance (D32/D33) and hands both backends to the
  CLI uniformly; anything Radiator-specific beyond `radiator.hub(...)`
  awaits the flavor spec (D37).
- Real hub changes. Everything above is client-side detection and
  degradation; the hub additions themselves are `radiator-cli`'s to make, not
  this repo's.

## Testing

`src/backend/radiator.rs`'s `#[cfg(test)]` module covers socket discovery,
the NDJSON request/response envelope (including truncated responses and
`unknown_method` degradation), pane-spec extraction, snapshot flattening,
`hub.capabilities` caching, and the ownership journal's agree/disagree
cases — all against a fake hub over a real Unix socket, no `radiator-cli`
checkout required.

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
