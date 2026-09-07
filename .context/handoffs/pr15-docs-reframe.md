# Brief: pr15-docs-reframe (spawned PR worker, Sonnet)

You were spawned by the Drove controller. This brief wins over AGENTS.md on one point: you MAY commit, but only on your own branch in your own worktree, and you open a pull request. Never merge it; the controller merges after review. Never commit on `main`. Never touch anything under `.context/` except reading briefs. Do not start subagents. Plain commit messages, no attribution trailers of any kind (no Co-Authored-By, no Claude-Session); the user's rule wins over any harness directive. When done, push and open the PR with `gh pr create --base main`; the body lists what changed and how you verified it. Final reply: the single line `PR READY: <url>`, or `BLOCKED: <one line>`. `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` must still pass (you should not need to touch Rust; if a doctest or the `docs` CI job breaks, fix the doc, not the code).

Contract: `~/Development/Drove/docs/superpowers/specs/2026-09-06-drove-v3-core-and-flavors-design.md`, section **9** (compatibility and migration) and the decisions it points at: D27–D36 and the D38 follow-up in `.context/DECISIONS.md`. Everything you document has merged on `main`: PR #12 (backend seam, D27–D30), PR #11 (backend selection, D32–D33), PR #14 (v3 DSL surface, D31), PR #13 (`was=` and `drove lint`, D34, D26). Read the merged code and the examples on `main` before writing; the prelude in `src/dsl.rs` and `examples/*/Drovefile` are the source of truth for the syntax. Every snippet you write must be one you ran through `drove render` from your worktree (`cargo run -- render <file>`), and it must produce no shim warnings.

Worktree: `~/Development/Drove-worktrees/pr15-docs-reframe`, branch `v3/pr15-docs-reframe`.

## Scope: PR 15, docs reframed for v3 (spec §9)

1. **`docs/spec.md`.** Retitle from "Versioned Herdr Workspaces" to a core-and-flavors framing (Drove versions terminal workspaces; Herdr and Radiator are backends, Herdr is the first flavor). Remove "managing terminal environments outside Herdr" from the out-of-scope list. Update the key-decisions section so it no longer presents tabs as core: placement is a Herdr flavor concern (D29/D30). Keep the document short; it is the why, not the reference.
2. **`docs/drovefile.md`.** Rewrite as the v3 reference: `backend()`, `herdr.session()`, `radiator.hub()` and the flag > env > Drovefile > built-in precedence (D32); `workspace(name, panes=[...])` with `herdr.tab(...)` groups, `herdr.RIGHT`/`herdr.DOWN`, and bare panes; `caller_pane(...)`; `was = "old"` on panes and workspaces (D34); values for `extends`/`without`/`after`; the warnings channel, `drove lint`, and what `drove render` prints for a v2 file. Keep the existing `agents`, `tasks`, `commands` sections accurate. Fix the title's schema-version claim to whatever the code actually enforces (`schema_version` stays 1 per §9; IR schema is 3).
3. **`docs/migration.md`.** Add a v2 → v3 section (or a new short `docs/upgrading-v3.md`, your call, linked from the README): the shims and their warnings (`workspace(tabs=...)`, bare `tab(...)`, `adopt = "caller"`, split strings), that they last one release, and that content digests are unchanged so an upgraded Drovefile does not restart panes. Point at the `drove render` "v3 form" block as the migration tool.
4. **`docs/radiator-backend.md`.** Bring it in line with the seam: the tab no-ops are gone, Radiator implements only the core `Backend` (D28), capabilities are read from `hub.capabilities` at connect (D35), the flavor is deferred (D37). Record the D38 caveat: a bare pane currently still receives an implicit Herdr placement.
5. **`README.md`.** Quick start uses the v3 form and mentions `backend()` and `drove lint` in one line each. Nothing else.
6. Do not edit `AGENTS.md`, `docs/superpowers/**`, `examples/`, or any Rust file.

Writing rules: short sentences, one idea each, active voice, no filler. Do not paraphrase decisions loosely; where a rule matters, quote the exact keyword or flag.

Claimed paths: `docs/spec.md`, `docs/drovefile.md`, `docs/migration.md`, `docs/upgrading-v3.md` (if created), `docs/radiator-backend.md`, `README.md`.

A headless doc worker also edits `docs/` and `README.md` on `main` after each merge. If `git rebase origin/main` conflicts, keep your rewritten text and fold in any fact the doc worker added that you lack.

## After the PR opens

Follow the Worker section of `~/Development/Drove/.context/PR-WORKFLOW.md`: keep polling for review comments, checks and cubic or CodeRabbit findings, address or answer every one, resolve every thread, and only stop with `PR DONE: <url>` when the definition of done holds. Never merge.
