# recon-forward-build — report

Paper: Spall, Mitchell, Tobin-Hochstadt, "Forward Build Systems, Formally",
CPP '22 (arXiv 2202.05328). Parsed with docling on `radiator-dgx`: the GPU run
died with `CUDA error: out of memory`; `CUDA_VISIBLE_DEVICES="" docling
--device cpu` succeeded, so the Agda listings below are a real docling parse.

---

## 1. The paper's model

### Forward scripts

A *backward* build system (Make) makes the user declare targets, dependencies and
rules. A *forward* build system makes the user write the build as an ordinary
program — a sequence of commands, like a shell script — and recovers the
dependency structure by tracing which files each command read and wrote. Memoize
and Fabricate scripts are Python; Rattle scripts are Haskell calling `cmd`.
Host-language control flow (`forM`, `let`) is invisible; only the `cmd` calls
are, so Rattle's view of a script is a flat list of commands.

### Definitions (from Figures 2–3)

```
FileName = String              FileContent = String
File = FileName × FileContent  FileSystem = FileName → Maybe FileContent
Memory = List (Cmd × List MaybeFile)
Cmd = String                   Build = List Cmd
CmdFunction = FileSystem → List File × List File
```

A command's effect is a function of the whole file system, but an `Oracle` maps
each `Cmd` to a `CmdFunction` paired with a `CmdProof` witnessing that only the
files it claims to read matter:

```
CmdProof f = ∀ s1 s2 → (∀ g1 → g1 ∈ reads f s1 → s1 g1 ≡ s2 g1) → f s1 ≡ f s2
```

That single formula *is* the tracing-correctness and determinism assumption of
§2.2, made formal. The reference semantics is the plain script:

```
script : Build → FileSystem → FileSystem
script []      sys = sys
script (x : b) sys = script b (run x sys)
```

### Correctness

> A forward build system is correct if, for every build, it either produces the
> same result as running the commands in order, or reports an error.

### Hazards

Three kinds, as `Hazard` constructors indexed on a `FileSystem`, the `Cmd` just
run, the *script* build, and a `FileInfo = List (Cmd × FileNames × FileNames)`
recording what each command read and wrote:

- **`ReadWrite`** (read-before-write): the command's writes intersect files read
  by earlier commands. `v ∈ cmdWriteNames x s → v ∈ filesRead ls → Hazard`.
- **`WriteWrite`**: the command's writes intersect files written by earlier
  commands.
- **`Speculative`** (speculative write-before-read): there are two commands
  `x1`, `x2` among those run, where `x2` read a file `x1` wrote, `x2 before x1`
  in the run order, `x2 ∈ b`, and `¬ x1 before x2 ∈ b` — i.e. `x1` was not
  *meant* to run before `x2`, possibly not meant to run at all.

`HazardFree s b1 b2 ls` is the inductive converse: each command is `¬ Hazard`,
and the rest is `HazardFree` after running it. Note the two build arguments —
the build actually run and the script build — which is what makes speculation
expressible.

The point of hazards: *absence of hazards implies the build has reached a fixed
point.* `gcc -c foo.c; echo X >> foo.c` never reaches one, and is exactly a
read-before-write hazard.

### Early cutoff and memoization

```
run? x (s , m) with x ∈? map proj1 m
  ... | no  x∉ = true
  ... | yes x∈ = is-nothing (maybeAll (get x m x∈))
runF cmd st = if (run? cmd st) then doRun st cmd else st
fabricate []      st = st
fabricate (x : b) st = fabricate b (runF x st)
```

A command is skipped iff it is in `Memory` **and** every recorded file still has
the same value. Fabricate/Memoize record only reads; Rattle's `doRunR` records
reads **and** writes.

### Theorems

- **`correct-fabricate`**: for `PreCond s b b` and `HazardFree s b b []`,
  `∀ f1 → proj1 (fabricate b (s , [])) f1 ≡ script b s f1`. The proof needs the
  `WriteWrite` freedom (a skipped command's outputs cannot have changed); it does
  *not* need `ReadWrite` freedom — that would be needed for idempotence — and
  not `Speculative`, since Fabricate does not speculate.
- **`script ≡ rattle-unchecked`**: needs only `DisjointBuild s b` (no command
  writes its own inputs), *not* hazard-freedom — because recording writes as
  well as reads makes the skip decision self-correcting.
- **`soundness`**: if `rattle` returns a file system rather than a `Hazard`, it
  equals `script br s`.
- **`completeness`**: if the build is `HazardFree`, `rattle` does *not* report a
  hazard. No false alarms.
- **`correct-rattle`**: `¬ HazardFree s b b [] ⊎ ≡toScript s b b`. Follows from
  soundness + completeness.
- **`reordered≡`**: for permutations `br`, `bs` with `HazardFree s br bs []`,
  `∀ f1 → script bs s f1 ≡ script br s f1`. Reordering a hazard-free build is
  invisible. This is the theorem that licenses parallelism.
- **`correct-speculation`** (Figure 7): `¬ HazardFree s bc bc [] ⊎ ≡toScript s br bc`
  — **not provable** for Rattle as implemented. Speculation can corrupt files
  that are inputs this run but were outputs last run, producing a build with a
  speculative hazard and no real hazard, which cannot be run script-equivalently.
- **`semi-correct`** (partial correctness, Figure 9): `¬ HazardFree s br bs [] ⊎
  ¬ HazardFree s bs bs [] ⊎ ≡toScript s br bs`. A speculation-induced hazard
  counts as a correct outcome, because Rattle's recovery is to rerun sequentially.

### The bug found (§6.4)

Rattle checked hazards only when a command *finished*, over only that command's
files, and never rechecked a command that was skipped-because-already-run. So a
command that was `speculated` at check time and became `required` later escaped
speculative-hazard checking. The model made it obvious: `required?` must be
tested against the full command set, not a prefix. Modeling speculation as "any
permutation" rather than encoding Rattle's actual heuristic is what made the bug
trivial to find.

### Comparison

| | records | speculates | correctness needs |
|---|---|---|---|
| Memoize / Fabricate | reads only | no | `HazardFree` (incl. WriteWrite) |
| Rattle sequential | reads + writes | no | `DisjointBuild`; hazards reported |
| Rattle speculative | reads + writes | yes, over last run's commands | only *partial* correctness |

---

## 2. Mapping onto Drove

Treat a Drovefile as a forward script whose commands are backend calls.
Substituting the "file system" for the backend's resource table:

| Drove operation | reads | writes |
|---|---|---|
| `create_workspace(label, cwd)` | repo root, workspace label namespace | `workspace/<logical-id>` (label, cwd, runtime id) |
| `apply_layout(ws_id, tab_id?, label, layout)` | `workspace/<id>` | `tab/<ws>/<id>`, and `pane/<pane-id>` for every pane in the tree |
| `rename_workspace` / `rename_tab` | the resource's runtime id | that resource's `label` field only |
| pane `command` | pane's PTY, `cwd`, files the process opens | pane's PTY, files the process writes |
| `start_agent(pane_id, …)` | `pane/<logical-id>`'s runtime id | `agent/<id>`, and the pane's PTY |
| `bootstrap.check` / `.run` | declared `inputs` + repo files touched | repo files (hooks, config), no backend state |
| `report_workspace_status` | `workspace/<id>` | a display field; effectively write-only, no reader |

Drove's `canonical_digest` over the desired spec is the `FileContent` of these
pseudo-files; `LocalState.profiles` is the `Memory`; `Plan.actions` is the
`Build`. The `journal` (`begin_action`/`finish_action`) is a partial `FileInfo`.

### What a hazard is here

- **Write-before-write.** Two declarations claiming one resource: two panes with
  the same logical id (Drove already rejects this in `Profile::validate`), or two
  profiles owning the same backend workspace. Also `RenameTab` followed by
  `ReplaceTab` on the same address — the second discards the first's write.
- **Read-before-write.** `StartAgent` reads `pane/<id>`'s runtime id; a later
  `ReplaceTab` on that pane's tab writes it. Today `action_priority` orders
  `ReplaceTab` (6) before `StartAgent` (7), which *avoids* the hazard by
  construction — that ordering is load-bearing and undocumented.
- **Read-before-write, the real one.** A `bootstrap` task that writes files a
  pane `command` reads: `RunBootstrap` is priority 0, so it wins, but nothing
  checks that a bootstrap task doesn't write into a directory a *running* pane's
  process already read. That is exactly `gcc -c foo.c; echo X >> foo.c`, and by
  the paper's definition means Drove never reaches a fixed point.
- **Speculative write-before-read.** Drove doesn't speculate, so this class is
  currently vacuous. It becomes live the moment Drove parallelizes tab creation
  or runs bootstrap tasks concurrently with layout application.
- **A rename racing a create** is the paper's *non*-hazard: the model assumes
  correct tracing and sequential execution. It is a Drove-specific failure the
  build-system model does not cover (see §3).

### Early cutoff

`run?` for Drove: skip an action iff the logical id is in `LocalState` **and**
the observed backend state for it still matches what was recorded. Drove's
`build_plan` already does the stronger thing — it re-reads observed state via
`ObservedState::gather` and compares normalized layout JSON — so it is closer to
`rattle-unchecked` (records writes, re-verifies them) than to `fabricate`.

Evidence Drove would need to record, per resource, to make cutoff sound:
- the **desired digest** (has, per `ManagedWorkspace.desired_digest`);
- the **observed digest** at apply time, i.e. the digest of the
  `ExportedLayout` the backend returned — currently the layout is used to
  populate `panes` but its digest is not stored;
- the **runtime ids** written (has: `workspace_id`, `tab_id`, `panes`);
- the **inputs read**: repo-relative `cwd`, the resolved argv, env, and for
  bootstrap the digests of `inputs` (has, in `BootstrapTask::digest`).

Missing observed digests is why `ReplaceTab` fires with reason "managed tab
layout could not be observed": Drove has the `Memory` entry but no recorded
value to compare against, so it falls back to the destructive action.

---

## 3. Where the analogy breaks

- **Outputs are live processes, not files.** A `File` is a value; a pane is a
  PTY with a process, scrollback, and a user's attention in it. `run x s` is
  idempotent-by-overwrite; `apply_layout` on an existing tab is not.
- **Re-running is destructive.** The model's central move — "if in doubt, run the
  command again, the result is the same" — is false. Drove marks `ReplaceTab`
  `destructive: true` and gates it behind `--allow-replace` precisely because
  re-running discards live PTYs. There is no build-system analogue of a command
  whose re-execution loses data the user wanted.
- **The backend is the state store.** Rattle reads the file system and can hash
  any file. Drove reads Herdr over a socket, gets whatever `snapshot` and
  `export_layout` expose, and must strip `pane_id`, `tab_id`, `focused` in
  `normalize_layout` to compare at all. `CmdProof`'s "result depends only on
  files claimed read" cannot be established: Drove cannot enumerate backend state.
- **Runtime identity is assigned by the writer.** `create_workspace` returns an id
  Drove did not choose; in the model, outputs are named before a command runs.
  Hence the journal: the crash window between "Herdr created it" and "Drove
  recorded it" has no build-system counterpart, where a file exists or does not.
- **Adoption has no analogue.** Taking over an already-running pane the user
  started is a command whose output already exists, with unknown provenance, and
  which must *not* be re-run. Rattle's answer would be to run it; Drove's must be
  to inspect and adopt.
- **Non-owned resources must survive.** `script` produces a whole `FileSystem`
  and equality is over all files, while Drove's spec preserves unmanaged
  workspaces. The right correctness statement for Drove is the §6.3 weakening:
  equality only over the resources the profile writes.
- **Concurrency is real, not modeled away.** The paper sequentializes parallelism
  by fiat ("all reads at the start, all writes at the end"). Drove faces a real
  concurrent mutator: the user, typing in Herdr while `drove up` runs.

---

## 4. A forward-style Drovefile

Current style is a value-returning DSL: `workspace(...)` builds a dict,
`profile(...)` emits it, identity is the explicit `id` field. Forward style would
be sequential calls with side effects, dependencies implicit in the handles.

```python
# forward style — sketch
def build(w):
    w.bootstrap(
        check = ["./scripts/install-hooks", "--check"],
        run   = ["./scripts/install-hooks"],
        inputs = ["scripts/install-hooks"],
    )

    control = w.workspace("control")
    coord = control.tab("coordinator")
    controller, eventlog = coord.split("down", 0.5)
    eventlog.run(["./scripts/eventlog-view"])

    control.tab("monitor", label = "monitor: system + agents") \
           .pane().run(["./scripts/system-monitor"])

    maint = w.workspace("maintenance")
    maint.tab("lazygit").pane().run(["./scripts/lazygit-or-shell"])

    for (tab_id, label, name, why) in REACTORS:
        maint.tab(tab_id, label = label).pane().run(
            ["./scripts/mock-reactor", name, why])

    if w.profile != "core":
        w.workspace("files").tab("files").pane().run(["./scripts/files-or-shell"])

REACTORS = [
    ("commit-reactor", "composer-2.5-fast: commit reactor",
     "commit-reactor", "watches result events and commits declared paths"),
    ("doc-sync", "sonnet: doc sync",
     "doc-sync", "watches committer acknowledgements and updates docs"),
]
```

`eventlog.run(...)` reads the pane handle written by `coord.split(...)`, which
read the tab handle written by `control.tab(...)`, which read the workspace
handle. The dependency graph is recovered from the calls, not declared — that is
the forward move.

**What the user gains.** Loops and conditionals over layout without building
lists of dicts. Profiles as a parameter (`w.profile`) instead of duplicated
`profile()` calls. No pane-id namespace to hand-maintain: `agent` could take the
handle, so `model.rs`'s "agent references unknown pane" check becomes a type
error at evaluation time. Panes that are never referenced never need names.

**What Drove must then compute or record.** Identity. Today `id` *is* the
ownership address, stable across edits and machines. In the forward style,
identity has to be derived — from call site plus iteration index, the way Rattle
uses the command string itself as the `Memory` key. Reordering two loop
iterations then silently retargets ownership, which is a write-write hazard the
user cannot see in the diff. Drove would have to either (a) keep explicit ids as
the first positional argument of every call, which recovers most of the current
verbosity, or (b) record a trace mapping call-site → runtime id and treat a
changed trace as a hazard requiring confirmation. It also loses cheap
`plan`-without-evaluation: the current DSL produces a value that can be digested
and diffed; a forward script must be *executed against a recording backend* to
know what it would do. That is precisely Rattle's design, and precisely why
Rattle needs tracing.

**Honest verdict.** The forward style is better ergonomics for generation and
worse for review. Drove's spec constraint — "shared definitions must produce
stable, useful code-review diffs" — argues for keeping the declarative form as
the file format, and adopting the forward model *internally*, for the planner.

---

## 5. Recommendations

1. **Compute hazards over the whole plan before executing any action, not per
   action as it completes.** §6.4: Rattle's real bug came from checking hazards
   only at command completion, over only that command's files, with a prefix
   view of what was `required`; a whole-build check makes the class of bug
   impossible. Drove's `build_plan` already has the whole action list in hand —
   add a hazard pass over it that reports read/write conflicts between actions as
   a first-class `Hazard` action kind, alongside `Conflict`.

2. **Record the observed digest of each applied resource, not just the desired
   digest.** `script ≡ rattle-unchecked` needs only `DisjointBuild`, while
   `correct-fabricate` needs full `HazardFree`, and the difference is exactly
   that Rattle records writes as well as reads. `store_tab` already receives the
   `ExportedLayout` the backend returned; digest it into `ManagedTab` so that
   "layout could not be observed" stops escalating to a destructive `ReplaceTab`.

3. **Make bootstrap inputs and pane working directories declare disjointness,
   and reject overlaps.** The model's `DisjointBuild` precondition (commands do
   not write their own inputs) is load-bearing in every proof, and the paper's
   own example of a build that never reaches a fixed point is a write to a file
   an earlier command read. A bootstrap task that writes a file a declared pane
   command reads is the same shape, and today nothing catches it.

4. **Never speculate or parallelize an action marked `destructive`.**
   `correct-speculation` is stated and *cannot be proven* — speculation can
   clobber files that are inputs now and were outputs before, yielding a build
   with no real hazard that still cannot be run script-equivalently. Panes are
   permanently that case: the previous run's output (a live PTY) is this run's
   input. If Drove ever parallelizes, restrict it to `CreateWorkspace`/`CreateTab`
   on disjoint addresses, where `reordered≡` applies.

5. **State Drove's correctness over the owned set only, and adopt the
   "equivalent, or report an error" contract verbatim.** §6.3 sketches exactly
   this weakening — equality over the files the script build writes, rather than
   over the whole file system — which is the formal version of Drove's ownership
   model. Writing it down gives `drove up` a testable specification: for every
   profile, either the owned resources match sequential application of the
   declared actions, or Drove exits non-zero with a named conflict; unowned
   resources are outside the equivalence relation entirely.
