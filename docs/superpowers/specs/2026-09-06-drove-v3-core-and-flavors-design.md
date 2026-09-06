# Drove v3 — core and flavors

Date: 2026-09-06. Supersedes the backend and DSL sections of
`2026-09-06-drove-v2-design.md` (D1–D26). Every decision here is numbered
D27–D37 so briefs can pin it. Decisions D1–D26 stay in force unless a
decision below replaces one by number.

## 1. Why

Drove reconciles a declared workspace onto a terminal backend. Two backends
exist: Herdr and the Radiator hub. They are meant to be co-equal, and Drove
must keep working on Herdr while Radiator is still being built.

Two problems block that today.

1. **The `Backend` trait is the union of every backend's verbs, not the
   intersection.** `create_tab`, `split_pane` and `set_ratio` are Herdr
   words in the core trait (`src/backend/mod.rs`). Radiator implements them
   as no-ops and silently flattens tabs. The `Capabilities` struct of eleven
   bools is the symptom: a runtime flag table over a type-level problem.
   The planner's `ActionKind` (`src/planner.rs`) repeats the union.
2. **The CLI can only reach Herdr.** `src/cli.rs` constructs `HerdrClient`
   and nothing else. There is no `--backend` flag. The Radiator backend is
   a module with tests, not a target `drove up` can use.

Smaller problems ride along: the DSL uses strings where it holds values
(`extends = "default"`, `split = "right"`, `adopt = "caller"`); the project
cannot declare which backend instance it targets, so a bare `drove up`
from a shell without `HERDR_SESSION` writes to the `default` session;
renaming a resource abandons its live pane; the Radiator backend
hardcodes capabilities the hub now reports; Windows binaries ship
untested.

## 2. Shape

Drove gets a **core** and **flavors**.

- The **core** is the intersection: what every backend must fully honor.
  Nouns: `backend`, `workspace`, `pane`, `task`, `profile`. Pane fields:
  `serve`, `cwd`, `env`, `ready`, `after`, `agent`, `on_start`, `on_stop`,
  `adopt`. A Drovefile that uses only the core reconciles on every backend
  with no `unsupported` outcomes.
- A **flavor** is one backend's own terminology: constructors, constants,
  actions and verbs that only that backend understands. Herdr's flavor
  holds `tab`, `split`, `ratios`, `agent start`. Radiator's flavor holds
  nothing yet beyond its target instance (D37).
- Using a flavor on a backend that does not implement it produces an
  `unsupported` outcome that `drove status` and `drove plan` report. It is
  never dropped silently.

## 3. Behavior: core trait, extension traits, accessor (D27, D28)

**D27 — `Backend` is the intersection.** The core trait keeps only verbs
every backend implements fully:

```rust
pub trait Backend {
    fn snapshot(&self) -> Result<SessionSnapshot>;
    fn caller_pane_id(&self) -> Option<String>;
    fn create_workspace(&self, label: &str, cwd: &Path) -> Result<String>;
    fn rename_workspace(&self, id: &str, label: &str) -> Result<()>;
    fn create_pane(&self, workspace_id: &str, spec: &PaneSpec) -> Result<String>;
    fn close_pane(&self, id: &str) -> Result<()>;
    fn rename_pane(&self, id: &str, label: &str) -> Result<()>;
    fn restart_command(&self, id: &str, argv: &[String]) -> Result<()>;
    fn prompt_agent(&self, id: &str, prompt: &str) -> Result<()>;
    fn process_info(&self, id: &str) -> Result<Option<ProcessInfo>>;
    fn report_tokens(&self, address: &str, tokens: &BTreeMap<String, String>) -> Result<()>;
    fn output(&self, id: &str, timeout: Duration) -> Result<String>;
    fn capabilities(&self) -> Capabilities;

    fn herdr(&self) -> Option<&dyn HerdrExt> { None }
    fn radiator(&self) -> Option<&dyn RadiatorExt> { None }
}
```

`create_pane` replaces `create_tab` + `split_pane` as the core way to make
a pane. `PaneSpec` carries `label`, `cwd`, `command`, `env`. On Herdr,
`create_pane` with no placement creates a pane in the workspace's first
tab; placement is a Herdr flavor concern (D29).

`Capabilities` shrinks to the graded core features only — the same verb
with partial support:

```rust
pub struct Capabilities {
    pub workspace_env: bool,
    pub pane_command_at_create: bool,
    pub metadata_tokens: bool,
    pub process_info: bool,
    pub events: bool,
    pub readiness_output: bool,
}
```

`tabs`, `splits_and_ratios`, `agent_start`, `agent_prompt` and
`adopt_caller` leave the struct. Tabs, splits and agent start become the
Herdr flavor; `prompt_agent` and adoption are core because both backends
implement them fully.

**D28 — flavors are extension traits reached through a typed accessor.**

```rust
pub trait HerdrExt {
    fn create_tab(&self, workspace_id: &str, label: &str, split: Split, ratios: &[f64]) -> Result<String>;
    fn split_pane(&self, tab_id: &str, spec: &PaneSpec, split: Split) -> Result<String>;
    fn set_ratio(&self, tab_id: &str, ratios: &[f64]) -> Result<()>;
    fn rename_tab(&self, tab_id: &str, label: &str) -> Result<()>;
    fn start_agent(&self, pane_id: &str, name: &str, kind: &str, args: &[String]) -> Result<()>;
}

pub trait RadiatorExt {}   // empty until D37 is revisited
```

`HerdrClient` implements `Backend` and `HerdrExt`, and overrides
`fn herdr(&self) -> Option<&dyn HerdrExt> { Some(self) }`. Every other
backend inherits the default `None`. The presence of the impl is the
capability; there is no bool for "has tabs".

Rules:
- The word `tab` may appear only in `HerdrExt`, `HerdrAction`,
  `Placement::Herdr`, the `herdr` prelude namespace, and Herdr's own
  client. A test greps the core modules and fails if it appears there.
- Flavors are closed. A Drovefile cannot declare a backend. Adding a
  flavor means adding one extension trait, one `Action` variant and one
  accessor; the exhaustive match in the executor forces every backend to
  answer the new variant.
- The two alternatives were weighed and rejected: enum dispatch over
  backends forces every `dyn Backend` call site to change for no gain at
  two backends; a generic IR over an associated placement type breaks the
  single serializable IR that `render`, the state file and digests need.

## 4. Data: nested actions and tagged placement (D29, D30)

**D29 — `Action` nests by flavor; placement is a tagged IR value.**

```rust
pub enum Action {
    Core(CoreAction),
    Herdr(HerdrAction),
    Radiator(RadiatorAction),
}
pub enum CoreAction {
    CreateWorkspace, RenameWorkspace, CreatePane, ClosePane, RenamePane,
    RestartCommand, AdoptPane, PromptAgent, RunTask, Detach, Conflict,
}
pub enum HerdrAction { CreateTab, RenameTab, SplitPane, SetRatio, StartAgent }
pub enum RadiatorAction {}
```

Each variant keeps the fields the v2 `ActionKind` carried. Ordering
ranks and phases (`RANK_*`, `PHASE_*`) are unchanged.

The executor is one exhaustive match. A flavor action on a backend whose
accessor returns `None` yields `Outcome::Unsupported { flavor, action }`,
which `drove plan` prints and `drove status` counts.

In the IR, a pane carries an optional placement:

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "flavor", rename_all = "snake_case")]
pub enum Placement {
    Herdr { tab: String, split: Split, ratios: Vec<f64> },
}
pub struct PaneResource { /* core fields */ pub placement: Option<Placement> }
```

The IR no longer has a `tab` resource kind. A Herdr tab is derived by the
Herdr flavor from the placements of the panes that name it. IR schema
version becomes 3.

**D30 — two digests: content and topology.** A pane's `drove_digest`
covers core fields only: `serve`, `cwd`, `env`, `agent`, `ready`,
`on_start`, `on_stop`. `placement` is excluded. Each placement group has
its own topology digest over `(tab, split, ratios, ordered pane names)`.

- Content change → `RestartCommand` in place (D22, unchanged).
- Topology change → replace, destructive and confirmed (D22, unchanged).
- Upgrading a v2 state file to v3 must leave every pane's content digest
  equal, so `drove up` after the upgrade proposes no restarts. A test
  compiles the v2 examples with the v3 compiler and asserts this.

## 5. DSL: namespaces, groups, values (D31, D32)

**D31 — flavors are prelude namespaces; groups place panes.**

The prelude gains two namespace objects, `herdr` and `radiator`. Core
constructors stay bare.

```python
control = workspace("control", panes = [
    herdr.tab("coordinator", split = herdr.DOWN, ratios = [0.5], panes = [
        caller_pane("controller"),
        pane("eventlog", serve = ["eventlog-view.sh", "-f"]),
    ]),
    herdr.tab("monitor", panes = [pane("agentmon", serve = ["htop"])]),
])
```

- `workspace(name, panes = [...])` is the core signature. The `panes`
  list accepts `pane` values and **groups**. A group is a flavor value
  that contains panes plus placement. The compiler flattens groups into
  the core pane list and writes each pane's `placement`.
- `herdr.tab(name, panes, split = herdr.RIGHT, ratios = [])` is the first
  group. `herdr.RIGHT` and `herdr.DOWN` are constants; the prelude also
  accepts the strings `"right"`/`"down"` for one release and warns.
- `caller_pane(name, ...)` replaces `adopt = "caller"`. The old form is
  accepted for one release and warns. At most one per profile (unchanged).
- `profile()` returns the value it registers. `extends`, `without` and
  `after` accept values or names. `extends = default` and
  `without = [files]` are the documented forms; strings stay valid.
- `workspace(tabs = [...])` is accepted for one release: the compiler
  rewrites it to `panes = [herdr.tab(...)]` and warns with the rewrite.
- `drove render` prints the v3 form of any v2 Drovefile it compiles.

**D32 — the project declares its backend and target instance.**

```python
backend("herdr")          # core: which backend this project reconciles onto
herdr.session("drove")    # Herdr flavor: which named session
radiator.hub("main")      # Radiator flavor: which named hub
```

A Drovefile may declare both flavor instances; only the active backend's
is used. Resolution order for the backend id and for the instance, most
specific first:

1. CLI: `--backend <id>`, `--target <name>` (`--session` stays as a Herdr
   alias of `--target`; `--socket` stays as an explicit override).
2. Environment: `DROVE_BACKEND`; `HERDR_SESSION` or `RADIATOR_HUB` per
   backend; `HERDR_SOCKET_PATH` as before.
3. Drovefile: `backend(...)`, `herdr.session(...)`, `radiator.hub(...)`.
4. Built-in: backend `herdr`; Herdr `default`; Radiator `main`.

The Drovefile is a default that an explicit flag or an ambient session
overrides. This is the Tilt `default_registry` model.

**D33 — backend selection is wired.** `src/backend/select.rs` exposes
`pub fn open(id: &str, target: &Target) -> Result<Box<dyn Backend>>`. The
CLI resolves per D32 and calls it. `drove up`, `status`, `plan`, `down`
and `run` all reach the Radiator backend when selected.

## 6. Rename without loss (D34)

**D34 — `was=` migrates identity.** A pane or workspace may declare
`was = "old-name"`. When the backend holds a resource whose `drove_name`
equals `old-name` and no resource named `new-name`, the planner emits
`RenamePane`/`RenameWorkspace` plus a token rewrite to the new name, and
no `Detach`. After one successful apply the declaration is inert;
`drove lint` (D26) warns when `was` matches nothing. This is the
Terraform `moved` block.

## 7. Backend catch-up (D35, D36)

**D35 — Radiator backend reads `hub.capabilities`.** Hub commit `80c0f1d`
landed `pane.set_metadata`, `PaneInfo.metadata`, `PaneInfo.process`,
`pane.tail`, `workspace.rename` and `hub.capabilities`. The backend calls
`hub.capabilities` on connect and sets `metadata_tokens` and
`process_info` from the reply. The local token journal is used only when
the hub reports no metadata support; when it does, the hub is
authoritative and the journal entry is dropped. `rename_workspace` calls
`workspace.rename` and errors if the hub refuses.

**D36 — Windows is tested.** Pane `cwd` normalization uses
`Path::components` and `MAIN_SEPARATOR`, never string replacement of
`/`. The CI matrix adds `windows-latest` to the test job. Reason: the
release workflow already ships Windows binaries; shipping untested is
worse than not shipping.

## 8. Deferred (D37)

**D37 — the Radiator flavor waits.** `radiator` exposes only `hub(...)`
now. Chat panes, runner state, the attention queue and plane-driven
tasks are designed in their own spec when the hub protocol for them is
stable. Also deferred to later specs: a `worktree` flavor for workspaces
spanning several repositories or worktrees; continuous reconciliation.

## 9. Compatibility and migration

- IR schema 2 → 3. Drovefile `schema_version` stays 1; the compiler
  accepts every v2 form for one release and warns with the v3 rewrite.
- State files from v2 load; pane content digests are unchanged (D30).
- `docs/spec.md` is retitled from "Versioned Herdr Workspaces" and
  "managing terminal environments outside Herdr" leaves its out-of-scope
  list. `examples/*` move to the v3 form.

## 10. Testing

Each decision names its test:

- D27/D28: a test greps `src/backend/mod.rs`, `src/planner.rs`,
  `src/ir.rs`, `src/executor.rs` for the identifier `tab` and fails on a
  hit. A fake backend with `herdr()` returning `None` receives
  `Herdr(CreateTab)` and the executor returns `Unsupported`; core panes
  in the same plan are still created.
- D29: planner tests from v2 pass with `ActionKind` replaced by the nested
  `Action`; ordering tests unchanged.
- D30: compile `examples/basic` and `examples/log-driven` with v2 and v3
  compilers; assert equal content digests per pane. Move one pane between
  tabs; assert content digest unchanged and topology digest changed.
- D31: prelude tests for `herdr.tab` flattening, `caller_pane`, value
  `extends`/`without`, each shim's warning text.
- D32/D33: a precedence table test over flag × env × file × built-in for
  both backends; `select::open("radiator", ..)` returns a backend whose
  `herdr()` is `None`.
- D34: declared `was` with a matching live token → plan has
  `RenamePane` and no `Detach`; without a match → lint warning.
- D35: fake hub replies to `hub.capabilities` with and without
  `metadata`; capabilities flip; journal used only in the second case.
- D36: the existing two planner tests pass on `windows-latest`.

## 11. Work split

Independent, start now:
- **PR 9 `pr9-windows`** — D36. Sonnet.
- **PR 10 `pr10-radiator-catchup`** — D35. Sonnet.
- **PR 11 `pr11-backend-seam`** — D27–D30. Opus 4.8. Rebases onto PR 9
  and PR 10 before opening.
- **PR 12 `pr12-backend-select`** — D32 (resolution) and D33. Sonnet.
  Adds `backend()`, `herdr.session()`, `radiator.hub()` to the prelude as
  the first members of the namespaces; PR 13 adds the rest.

After PR 11 lands:
- **PR 13 `pr13-dsl-v3`** — D31, migration shims, examples. Opus 4.8.
- **PR 14 `pr14-was-rename`** — D34 and the lint. Sonnet.

After PR 13 lands:
- **PR 15 `pr15-docs-reframe`** — section 9 docs. Sonnet.
