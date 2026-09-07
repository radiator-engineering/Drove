# Audit: the Herdr boundary

Scope: `src/backend/herdr.rs` (`HerdrClient`), `src/backend/select.rs` (D46
resolution), `src/cli.rs` (`up_command`/`down_command`/`resolve_backend`),
`src/executor.rs` (`up`), `tests/herdr_contract.rs`. Read whole call paths for
every socket call and shell-out, cross-checked against the live `herdr`
binary (0.8.2) where possible.

## Ranked findings

### 1. `drove up` against a session that must be freshly started applies against stale local state, then either no-ops or hard-aborts — CONFIRMED

- `src/cli.rs:619-627` (D48 prune step) + `src/executor.rs:668-680`
  (`ensure_session`) + `src/executor.rs:758-787` (`focus_first_workspace`).

D48's prune-before-plan protection (`prune_missing`) only runs when the
up-front `client.snapshot()` succeeds (`cli.rs:620 if let Ok(live_snapshot)
= &live_snapshot`). When the target isn't reachable *yet*, the comment at
`cli.rs:613-617` says there's "nothing live to prune against" and local
state is trusted as-is. But `up()` (`executor.rs:671-680`) then calls
`ensure_session`, which — when `ping()` fails — shells out to `herdr server
--session NAME` and waits for it to come up (`HerdrClient::ensure_session`,
`herdr.rs:634-647`). That path is exactly "the session was stopped/crashed
and Herdr wipes its workspace/id counter on a fresh start" (the same
scenario D48 already handles for the *reachable* case, per the comment at
`herdr.rs`/`cli.rs` referencing issue 24/D48). The unreachable-at-probe-time
case is the same scenario one snapshot call earlier, and it isn't pruned.

Trigger: a Herdr session that was stopped/crashed (socket unreachable) with
local state still recording old backend ids for it. Run `drove up`.
`live_snapshot` is `Err`, so no prune happens and `state.profile(...)` keeps
the stale ids. `build_plan` sees those ids as already owned and converged,
so the plan can come out `was_in_sync == true` — the resources are treated
as present in the brand-new (empty) session.

- If `focus_workspace` was not requested for that run, `up` reports
  `AlreadyRunning` and exits 0, having created nothing in the actually-empty
  session — a silent no-op.
- If a workspace *is* being focused (the common case — `up` defaults to
  focusing the profile's first workspace), `focus_first_workspace`
  (`executor.rs:758-787`) falls back to the stale `state` backend id
  (`applied.workspace_ids` is empty because no actions ran) and calls
  `ext.focus_workspace(&backend_id)` against the new session. Herdr returns
  an error for an unknown workspace id, `request()` turns that into
  `bail!(...)` (`herdr.rs:102-104`), and the `?` at `executor.rs:702`
  propagates all the way out of `up()` — the whole `drove up` run fails with
  a generic "Herdr API error" instead of reconciling (recreating the
  workspace, then focusing the one it just created).

Confirmed by reading: no test exercises "session unreachable at the
up-front probe, then `ensure_session` starts it" against non-empty stale
local state — `tests/cli.rs::up_saves_the_pruned_managed_set_before_applying`
only covers the *reachable* prune path (`serve_snapshot_then_ping`, i.e.
`session.snapshot` already answers). There is no fixture where the first
`session.snapshot` connect fails and a second, post-`ensure_session` probe
succeeds.

Smallest fix: after `ensure_session` reports `Started` (session was not
running and Drove just brought it up), re-snapshot and run the same
`prune_missing`/save step used at `cli.rs:619-627` before building the plan
— or, simpler, treat `Started` as "local state for this profile is now
fully stale" and prune everything unconditionally. Either way, the plan
must be built from a snapshot taken *after* the session is known to be the
one that will receive the apply.

### 2. Ambient/explicit session name `"default"` resolves to the wrong socket path — CONFIRMED

- `src/backend/herdr.rs:897-920` (`resolve_socket_path`) vs.
  `src/cli.rs:893-897` (`resolve_backend`'s `AmbientEnvInputs`).

Herdr's actual "default" (unnamed) session lives at the bare
`~/.config/herdr/herdr.sock`, not under `sessions/default/` — confirmed
against the live `herdr session list` output:
```
default   running   /Users/jjmartin/.config/herdr    /Users/jjmartin/.config/herdr/herdr.sock
drove     running   .../sessions/drove               .../sessions/drove/herdr.sock
```
`resolve_socket_path` knows this: its *ambient env* fallback explicitly
special-cases the literal value `"default"` (`herdr.rs:910-918`, `session
!= "default"`) so that an ambient `HERDR_SESSION=default` falls through to
the bare path instead of `sessions/default/herdr.sock`. That special case
only fires in the branch taken when `session: Option<&str>` is `None`
(i.e., nothing named a session at any D46 level) and the function falls
back to reading `HERDR_SESSION` from the environment itself.

But `resolve_backend` (`cli.rs:893-897`) reads `HERDR_SESSION` *before*
`herdr.rs` ever gets a chance to apply that exception, and hands the raw
value straight into `select::resolve`'s ambient level
(`AmbientEnvInputs.herdr_session`). `select::resolve` (`select.rs:126`)
puts it into `Target.name` unfiltered, so `Target.name == Some("default")`.
`select::open` (`select.rs:147-150`) then calls
`HerdrClient::discover(socket=None, session=Some("default"))`, which hits
`resolve_socket_path`'s *first* branch (`herdr.rs:901-906`, `if let
Some(session) = session`) — the one with no `"default"` exception — and
returns `sessions/default/herdr.sock`. That path does not exist for the
real default session, so Drove will report "not running" against a session
that is, in fact, up.

This isn't limited to the ambient case: the exact same wrong path is
produced whenever a session name of literally `"default"` reaches
`Target.name` through *any* D46 level — `--session default`,
`DROVE_SESSION=default`, a profile's `session = "default"`, or
`herdr.session("default")` in the Drovefile — since none of those levels
filter the string either. The exception written into `resolve_socket_path`
is effectively dead code in the CLI's normal flow: it only fires when
`HerdrClient::discover` is reached with `session: None` and *that* code
path re-reads `HERDR_SESSION` itself — which requires every D46 level above
ambient to be absent *and* the ambient level itself to not have already
been captured into `Target.name` by `resolve_backend`. In practice
`resolve_backend` always captures it first, so the exception never runs for
a real CLI invocation.

Reproduction: from inside Herdr's actual default session (`HERDR_SESSION`
unset because you're not in a *named* session, or explicitly exported as
`default`), or with a profile declaring `herdr.session("default")`, run
`drove status`. It connects to `~/.config/herdr/sessions/default/herdr.sock`
(missing) instead of `~/.config/herdr/herdr.sock` (the real one) and
reports "not running".

Smallest fix: give `select.rs` (or `resolve_socket_path`'s first branch)
the same `session != "default"` treatment the env-var fallback already has,
so a resolved target name of exactly `"default"` is normalized to "no named
session" (`Target.name = None`) at the point `Target` is built, once,
rather than requiring every call site to know about the exception.

### 3. `snapshot()` turns "a pane closed mid-snapshot" into a hard error for the whole command — PLAUSIBLE

- `src/backend/herdr.rs:112-125`.

`snapshot()` first calls `session.snapshot`, then loops over every pane it
lists and calls `pane.process_info` for each with `?` (line 121). Between
those two round trips, a pane can be closed by the user (or by a
concurrent `drove down`/reactor) — a real race for any long-lived Herdr
session with humans and agents in it. If Herdr's `pane.process_info`
returns an error for a pane id that no longer exists (rather than a
null/empty result), that one `?` fails the *entire* snapshot, which
`status`, `plan`, and `up`'s up-front probe (`cli.rs:306`, `cli.rs:619`)
all depend on — turning a single stale pane into "not running" for the
whole profile instead of a snapshot that simply omits (or nulls) that pane.
This mirrors the exact bug shape named in the brief (trusting a listed id
that went stale between two calls, and aborting instead of reconciling),
but I could not confirm Herdr's actual error code for `pane.process_info`
against an unknown pane id from the outside, so this is plausible rather
than confirmed.

Smallest fix: treat a `pane.process_info` failure for a specific pane as
"no process info for this pane" (log/skip) rather than failing the whole
snapshot, or accept the race as out of scope but document it.

### 4. A stray line before Herdr's `--json` error payload flips "already stopped" into an unrecoverable `down` failure — PLAUSIBLE

- `src/backend/herdr.rs:748-761` (`stop_failed_because_not_running`).

The D47 "already stopped" detection requires `stdout`/`stderr` to parse as
a *bare* JSON value (`serde_json::from_str(text.trim())`). If a future (or
differently configured) `herdr session stop NAME --json` ever emits any
non-JSON preamble on the same stream before the JSON object — a deprecation
notice, a channel-update nudge, anything on stderr sharing the stream this
function reads — the parse fails, `stop_failed_because_not_running`
returns `false`, and `run_stop_session` treats a merely-already-stopped
session as an undocumented failure and `bail!`s, leaving `drove down`
exiting non-zero for a case D47 was written to make idempotent. This is a
forward-looking/format-fragility gap rather than a currently-reproducible
bug (0.8.2's actual stderr for this case is exactly one JSON object, per
the existing test fixtures), so plausible rather than confirmed.

Smallest fix: search for the first `{`...last `}` span in the text rather
than requiring the whole trimmed stream to be valid JSON, or scan
line-by-line for one that parses.

### 5. No protocol/version check on `SessionSnapshot` — PLAUSIBLE

- `src/backend/herdr.rs:778-796` (`version`, `protocol` fields, unused
  anywhere else).

`session.snapshot` responses carry `version`/`protocol`, but Drove never
compares them to anything. Pointed at an old or mismatched `herdr` binary
via `HERDR_BIN_PATH` (explicitly named as an audit dimension), a
field-shape change would surface only as an opaque `.context("... response
omitted ...")` error (or, worse, silently parse into defaults via
`#[serde(default)]` and produce a wrong plan) rather than a clear
"HERDR_BIN_PATH points at an incompatible Herdr (vN, protocol P)"
diagnostic. Not a reproducible bug against 0.8.2, so listed as a gap rather
than a confirmed defect.

## Already fine

- **Stale/removed socket files**: Drove never special-cases "session listed
  as stopped" vs. "socket file missing" vs. "stale socket file with nothing
  listening" — it always just tries to connect and treats any failure the
  same way (`ensure_session`'s `ping().is_err()` → try to start). That's
  the right call; Herdr's own connect failure is ground truth and needs no
  extra classification on Drove's side.
- **`--socket` precedence**: `resolve_socket_path`'s explicit-socket branch
  is checked first, unconditionally, ahead of every named-session logic
  (`herdr.rs:897-900`) — verified this can't be shadowed by a session name
  from any D46 level, since `Target.socket` and `Target.name` are carried
  and consumed independently through `select::open`.
- **`stop_session`'s stop-then-delete ordering and the "undocumented
  failure never reaches delete" invariant** (`run_stop_session`,
  `herdr.rs:706-737`) are both correctly implemented and covered by
  dedicated tests (`stop_session_stops_then_deletes_in_order`,
  `stop_session_with_an_undocumented_stop_failure_is_an_error_and_never_deletes`).
- **NDJSON response timeout/truncation handling** (`read_line_with_timeout`,
  `herdr.rs:951-979`): bounded via a helper thread + channel on every
  platform (not just where socket-level recv timeouts work), and a
  zero-byte read or a line missing its trailing `\n` are both treated as
  errors rather than silently accepted as a valid (possibly truncated)
  response. Well covered by `output_times_out_on_a_withheld_response` and
  `rejects_truncated_ndjson_response`.
- **D46 precedence order itself** (`select.rs::resolve`): the six-level
  order (flag > explicit env > profile > file > ambient > built-in) for
  both backend id and target name, and the `--session`/`DROVE_SESSION`
  Herdr-alias-ignored-for-Radiator rule, are exhaustively table-tested in
  `select.rs`'s own test module and match the module doc comment exactly.
  The only defect found in this area is finding 2 above (the `"default"`
  string itself, not the precedence order).

## Harness gaps (most likely to hide a bug, first)

1. **The fake Herdr in `src/backend/herdr.rs`'s and `tests/cli.rs`'s unit
   tests is hand-scripted per test and only ever answers with success
   responses shaped exactly the way the code under test expects.** No test
   anywhere sends back a Herdr-shaped *error* response
   (`{"error": {"code": ..., "message": ...}}`) from `session.snapshot`,
   `pane.process_info`, `layout.apply`, etc. — only `ping`/`pane.read`
   exercise timeout/truncation failures. This is exactly why finding 3
   (a mid-snapshot `pane.process_info` failure) can't be confirmed from the
   test suite alone: nothing in the harness can produce that response
   shape to see what `HerdrClient` does with it.
2. **`tests/herdr_contract.rs` only asserts that method *names* appear in
   `herdr api schema --json`'s output text** — it does not validate request
   or response field shapes (`root_pane` vs `tab`, presence/absence of
   `tab_id`, error code strings like `session_stop_failed`) against the
   real schema at all. A Herdr release that renames a field Drove reads via
   `find_string`/`.get(...)` (e.g. `workspace.create`'s `root_pane`/`tab`
   duck-typing at `herdr.rs:136-156`) would pass this contract test and
   still break Drove silently. It's also skipped outright (`_ => return`)
   whenever `herdr` isn't on `PATH` or the subcommand doesn't exist yet, so
   CI can pass with this check never having run at all.
2b. Same file offers no coverage of `resolve_socket_path` against a *real*
    `herdr session list` shape — finding 2 (`"default"`'s wrong path) would
    have been caught immediately by a test that runs `herdr session list`
    and compares its printed socket for the running default session against
    `resolve_socket_path`'s computed path for `HERDR_SESSION=default`.
3. **No test starts two Herdr clients against the same session name
   concurrently to exercise the `ensure_session` "two `drove up` at once"
   race** the brief calls out. I could not find evidence this currently
   causes a user-visible bug (the loser's `start_session_server` spawns a
   process that fails to bind and exits, but the winner's socket still
   answers `wait_for_ping`), so I'm not reporting it as a finding — but the
   harness has no way to prove that reasoning either.
4. **`stop_failed_because_not_running`'s tests only feed it clean,
   single-object JSON on stdout/stderr** (finding 4) — never a stream with
   a leading non-JSON line, never a JSON array instead of an object, never
   the code nested somewhere other than top-level/`error.code`.
