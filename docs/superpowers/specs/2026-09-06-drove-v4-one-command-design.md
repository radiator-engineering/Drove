# Drove v4: one command, named profiles

Date: 2026-09-06. Decisions D41–D45. D1–D40 stay in force. Supersedes the
quick-start shape in the v3 spec: `drove` is the command, the rest are tools.

## 1. Problem

Getting a project's workspace onto the screen takes several commands and a
running Herdr session, and `drove up` today only runs tasks: the
workspace/pane actions the planner emits are printed, never applied
(`src/cli.rs`, `Command::Up`). Profiles exist but share one file-level
backend and session, and are selected by a flag.

## 2. Decisions

**D41 — profile-scoped target.** `profile(name, session = None,
backend = None, ...)`. A profile may name the Herdr session (or, for
`backend = "radiator"`, the hub) it lives in and the backend it reconciles
onto. Unset fields inherit the file-level `backend(...)`,
`herdr.session(...)`, `radiator.hub(...)`. Resolution order per D32 becomes
flag > env > profile > file > built-in. `extends` copies the parent's
`session`/`backend` unless overridden. Model: `Profile { session:
Option<String>, backend: Option<String> }`. `select::resolve` takes the
profile's values as an extra layer between env and file.

**D42 — the profile is positional.** `drove [PROFILE]` and
`drove <SUBCOMMAND> [PROFILE]`. `--profile NAME` stays as an alias (no
warning). With no profile given: the profile named `default` if declared,
else the file's only profile, else exit 2 listing the profiles. Unknown
name: exit 2 with the list. `drove ls` prints every profile with backend,
target name, and whether the target is reachable (`ping` on the resolved
socket). `--json` gives the same as an array.

**D43 — `up` gets you there.** `drove [PROFILE]` (`up`):
1. Resolve backend and target (D41).
2. If the target is not reachable and the backend is Herdr: start the
   session's server headlessly if the Herdr CLI can (`herdr server` with
   the session name; the PR verifies this against the installed Herdr and
   documents what it found); wait for the socket; else fail with the exact
   `herdr --session NAME` command to run. Radiator: fail with the hub name.
3. Apply the plan, not only its tasks: wire `executor::apply_plan` (already
   implemented, never called from the CLI) into `up`, after tasks, with the
   same `--yes` gate for destructive actions. `Conflict` still exits 2.
4. Focus: bring the profile's first declared workspace (or `--workspace
   NAME`) to the front through `workspace.focus`. When the caller's
   terminal is outside Herdr (`HERDR_ENV` unset) and stdout is a TTY, exec
   `herdr session attach NAME` so the user lands in it. `--no-focus`
   skips both. `--json` implies `--no-focus`.
5. Print one line: `profile dev: N created, M changed, K tasks run, in
   sync` or `profile dev: already running, brought to front`.
`plan`, `status`, `render`, `lint`, `run`, `down` are unchanged except for
taking the positional profile.

**D44 — HerdrExt grows session and focus verbs.** `HerdrExt::focus_workspace(id)`
and `HerdrExt::ensure_session(name) -> Result<SessionState>` (`Running`,
`Started`, `CannotStart { hint }`). `ensure_session` shells out to the
`herdr` binary (`HERDR_BIN_PATH` or `PATH`); Radiator gets nothing (D37).

**D45 — Drove dogfoods itself.** The repository root gets a `Drovefile`
declaring the log-driven layout this repo actually runs (control,
maintenance, files; session `drove`) as profile `default`, plus a
`monitoring` profile in session `drove-mon` with one workspace holding
the eventlog viewer and `git log --oneline`. `drove` in this repo brings
the controller layout up; `drove monitoring` opens the second session.
README quick start becomes: write a Drovefile, run `drove`.

## 3. Tests

- D41: model validation (`session`/`backend` per profile; `extends`
  inherits; override wins); `select::resolve` precedence table gains the
  profile layer (flag > env > profile > file > built-in) for both backends.
- D42: positional and flag forms resolve the same profile; no-profile
  rules (default / only / exit 2 with list); `drove ls` text and JSON on
  the log-driven example with an unreachable target.
- D43: with a fake Herdr, `up` applies `CreateWorkspace`/`CreatePane`
  actions (recorded on the fake), then focuses the first workspace; with
  `--no-focus` no focus call; already-in-sync prints the second summary
  form; unreachable target with a session the fake cannot start returns
  the hint and exit 1.
- D44: `focus_workspace` sends `workspace.focus` with the id; `ensure_session`
  on a running socket returns `Running` without spawning anything.
- D45: `cargo run -- render` on the repo Drovefile has no warnings; `drove
  plan` against the live `drove` session reports in sync or only additive
  drift (recorded in the PR body, not a CI test).

## 4. Work split

- **PR 16 `pr16-profile-targets`** — D41, D42 (incl. `ls`). Sonnet.
  Paths: `src/model.rs`, `src/dsl.rs`, `src/backend/select.rs`, `src/cli.rs`
  (argument parsing, profile resolution, `ls` only).
- **PR 17 `pr17-one-command-up`** — D43, D44. Opus 4.8. Paths:
  `src/backend/mod.rs`, `src/backend/herdr.rs`, `src/backend/radiator.rs`
  (accessor default only), `src/executor.rs`, `src/cli.rs` (the `up` path
  only). Runs in parallel with PR 16; second to merge rebases.
- **PR 18 `pr18-dogfood`** — D45 and docs. Sonnet. After both.
