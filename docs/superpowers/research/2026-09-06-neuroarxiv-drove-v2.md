# NeuroArxiv pass: prior art for Drove v2

Date: 2026-09-06. Companion to `docs/superpowers/specs/2026-09-06-drove-v2-design.md`.
Method: real arXiv export-API fetches per category, one isolated read per paper, then one pick.
The Rattle paper (arXiv 2202.05328, "Forward build systems, formally") was read separately and is already folded into the spec as the planner's hazard model.

## Searched

| Category | Terms | Papers kept |
|---|---|---|
| cs.SE | infrastructure as code | 3 |
| cs.SE | developer environment, reproducible environment | 1 |
| cs.DC | declarative, control loop, operator | 2 (query drifted to fog/edge; noted) |
| cs.PL | configuration language, Nix | 3 |
| cs.MA | LLM agents, coordination, workflow | 3 |

Twelve papers read. Discarded before reading as off-target: Cactus configuration language, IceCube environment, CIDER, Dafny IDE, cloud service recommender, EdgeWise MILP, DevOps SCM experience, stock-research orchestration.

## Papers read, by cluster

**A. Content-addressed identity and staged description**
- 1305.4584 Functional Package Management with Guix. Package identity is a hash of every declared input; Scheme generates the graph. `[rel8 prac8 rig8]`
- 1709.00833 Code Staging in GNU Guix. Host code builds the script that later runs elsewhere; cross-stage references tracked automatically. `[rel6 prac5 rig7]`
- 2608.04270 CURATE. Agents compose workflows from a catalog of reusable modules so a workflow can be rebuilt identically. `[rel5 prac6 rig5]`

**B. Configuration-language semantics and defect classes**
- 1608.04999 μPuppet. Formal operational semantics for a declarative subset, checked against the real tool on a corpus. `[rel7 prac7 rig9]`
- 1809.07937 Bugs in Infrastructure as Code. Defect taxonomy from real Puppet scripts; idempotency and dependency defects beyond syntax. `[rel6 prac8 rig8]`
- 1807.04872 Where Are The Gaps? Mapping study; testing and defect analysis of IaC scripts are thin areas. `[rel4 prac5 rig6]`

**C. Explicit shared state and harness layering for agents**
- 2406.09577 A New Generation of Intelligent Development Environments. The environment holds task state and verification results as explicit artifacts shared by humans and agents. `[rel7 prac5 rig3]`
- 2604.08224 Externalization in LLM Agents. Memory, skills, protocols, and a harness layer that enforces reliability across them. `[rel6 prac5 rig4]`
- 2607.14896 StructureClaw. Shared artifact store; every tool call logged against the artifact ids it touched. `[rel5 prac6 rig6]`

**D. Unifying abstractions over heterogeneous targets**
- 2108.07842 Infrastructure in Code (Kotless). Derive deployment description from annotated source by static analysis. `[rel4 prac6 rig6]`
- 2608.08400 Cloud-Edge Infrastructure Complexity. Practitioner pain points; recommends one platform-agnostic deployment description. `[rel5 prac4 rig5]`
- 2501.09964 Declarative Application Management in the Fog. Fully decentralised local rules, no central planner. `[rel2 prac3 rig6]`

## Prior-art pitfalls

- Guix: a purely functional model assumes deterministic, side-effect-free steps. Live panes and agents are neither. Use content hashing for *identity of the declaration*, never as proof the live thing matches.
- Bugs in IaC: idempotency and dependency-ordering defects are the classes syntax checks miss. A Drovefile validator that only checks shape will let those through.
- μPuppet: any semantics Drove documents is only true for the subset it formalises. Keep the prelude small and test it against the real backend on a corpus.
- Declarative fog: emergent, controller-less convergence is hard to reason about as one global diff. Drove should stay a central planner.
- CURATE: catalog-and-recompose was only shown with a human approving compositions. Do not assume unattended recomposition is safe.
- Externalization review: no evidence about several agents writing shared state at once. The event log's single-writer rule stays.

## THE PATH: content-addressed declarations (cluster A)

**Sketch.** Every resource in the IR gets a `digest`: the SHA-256 of its canonical JSON with children, prompts, env, argv, cwd and readiness included and backend ids excluded. The backend stores three ownership tokens per pane, `drove_name`, `drove_profile`, `drove_digest`, through `report_metadata`. Drift detection becomes a token comparison: same name and same digest means converged, same name and a different digest means "declaration changed, plan a restart or replace", no token means unmanaged. Process inspection is only a secondary signal for panes whose command exited. Prompts are compiled into the IR at `drove render` time, whether written inline or pulled from a repo file, so the IR is self-contained and its digest is stable. Bootstrap task approval already keys on a digest of the task's bytes; the same digest scheme now covers every resource, so one mechanism serves approval, drift and the state journal. Profiles that `extends` another profile recompute digests after resolution, so a preset used in two profiles produces two independent identities.

**Citations.**
- 1305.4584 Functional Package Management with Guix, https://arxiv.org/abs/1305.4584 : primary mechanism.
- 1709.00833 Code Staging in GNU Guix, https://arxiv.org/abs/1709.00833 : supporting evidence for compiling scripts into the description.
- 1608.04999 μPuppet, https://arxiv.org/abs/1608.04999 : validation method (corpus against the real backend).
- 1809.07937 Bugs in Infrastructure as Code, https://arxiv.org/abs/1809.07937 : failure modes to lint for.
- 2501.09964 Declarative Application Management in the Fog, https://arxiv.org/abs/2501.09964 : failure mode to avoid (decentralised convergence).

**First step.** In PR 1, add `digest()` to every IR resource and a unit test that reordering keys, changing a child prompt, or changing `cwd` changes it, while changing a backend id does not.

**Load-bearing risk.** Backends that cannot store tokens (Radiator today) get no digest, so drift falls back to the local state journal. If the journal and the hub disagree, Drove must trust the hub and mark the pane `unknown` rather than converge blindly.

**Avoid.**
- Hashing the repo root path into a digest. Moving a checkout must not invalidate every approval.
- Treating a matching digest as proof the process is healthy. Readiness probes stay separate.
- A validator that stops at shape. Lint for tasks without `check` and for `after` cycles.
- Letting reconciliation logic bleed into backends. The harness (planner) owns convergence.
- Emergent, per-pane convergence without a plan.

## Alternates considered, not chosen

- **B, semantics and lint.** Right method, but it improves confidence in the compiler rather than changing what Drove does. Adopted as testing practice (a Drovefile corpus run against the real backend in PR 6) and a later `drove lint`.
- **C, shared-state harness.** Already what the event-log workspace does. The papers confirm the split but give no mechanism beyond what the log has.
- **D, unifying abstraction.** Motivates the IR and the capability matrix, which the spec already has. Kotless's "derive config from source" does not apply: a workspace is not in the application source.

## Open thread

When a pane's digest changes, is the right move a destructive replace or an in-place restart of the command? Rattle says a changed input invalidates the trace, but a terminal pane holds scrollback the user may want. The spec should pick "restart in place, keep the pane" for `pane(serve=)` and "replace" only for topology changes. Check this in design review before PR 2.

## Answers to the spec's open questions

1. **Radiator readiness.** `port()` and `cmd()` probes run on the host and work on any backend. `output()` needs the backend to read pane output; Radiator lacks that, so its capability matrix has `readiness_output = false` and the planner reports such probes as `unsupported` instead of blocking.
2. **No caller to adopt.** If `drove up` runs outside a managed pane, the `adopt="caller"` pane is created as an ordinary pane, and the state journal records `adopted = false`. Drove prints one line saying so.
3. **Prompt inline or file.** Both. `prompt="..."` or `prompt=file("prompts/x.md")`, repo-relative, read at compile time into the IR so the digest covers it.
