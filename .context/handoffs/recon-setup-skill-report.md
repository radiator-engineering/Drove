# Recon: setup-log-driven-workspace capability inventory

Source: `~/.claude/skills/setup-log-driven-workspace/` (+ called scripts in event-log-coordination, herdr-layouts), compared to `docs/migration.md` and `docs/drovefile.md`.

---

## 1–2. Side effects in execution order (with classification)

### Phase A — `setup.sh` (files only; no herdr)

| # | Side effect | Class | Notes |
|---|-------------|-------|-------|
| A1 | Prereq checks: git repo, `EVENTLOG_SKILL/scripts`, `jq`, `git` on PATH | (g) validation | Warns if `cursor-agent`, `claude`, `timeout` missing |
| A2 | `safety-check.sh --doctor` → `link-scripts.sh` (append-event.sh, protect-log.sh on PATH); `install-guard.sh` (PreToolUse `eventlog-guard.sh` in `~/.claude/settings.json` and `~/.claudewho-*/settings.json`) | (b) bootstrap | Plumbing auto-healed when non-interactive; OS protect reported only unless `--yes` |
| A3 | `init-eventlog.sh` if missing: touch `.context/events.jsonl`; write `.context/EVENTLOG.md`; gitignore log + `.lock` | (a)+(b) | Log/lock ignored; EVENTLOG.md tracked |
| A4 | Template install → `.context/bin/{cursor-commit-reactor,doc-sync-reactor,run-reactor}.sh` (0755) | (a) | Skipped if dest exists unless `--force`; `--orders` skips A4–A6 reactor/hook scaffold |
| A5 | Template install → `.context/handoffs/{cursor-committer,doc-worker}.md` | (a) | Worker briefs |
| A6 | `.githooks/commit-msg` + `git config core.hooksPath .githooks` | (a)+(b) | Local git config; strips `Co-authored-by: Cursor\|Composer` |
| A7 | `.context/bin/controller-stop-hook.sh` | (a) | Stop hook script |
| A8 | Splice marker block into `AGENTS.md` from `templates/CONTROLLER.md` (`<!-- log-driven-workspace:start/end -->`, `{{DOC_PATHS}}`) | (a) | Refresh only with `--force` or `--orders` |
| A9 | `CLAUDE.md`: remove duplicate block; ensure `@AGENTS.md` import line | (a) | |
| A10 | Register Stop hook in `.claude/settings.json`: `bash "$CLAUDE_PROJECT_DIR/.context/bin/controller-stop-hook.sh"` under `hooks.Stop` | (b) | Idempotent jq append; new session required |
| A11 | Early exit if `--orders` (A8–A10 only) | (g) lifecycle | |
| A12 | Write `.context/workspace.env` if absent (MODEL, DOC_MODEL, DOC_PATHS, PASS_TIMEOUT defaults) | (a) | **Never regenerated**; existing file wins over CLI `--doc-paths` for AGENTS block |
| A13 | Write `.context/DECISIONS.md` from template if absent (`{{COMMITTER_MODEL}}`, etc.) | (a) | **Never regenerated**, even `--force` |
| A14 | Append `.gitignore` lines (idempotent): `.context/events.jsonl`, `.context/events.jsonl.lock`, `.context/*.reactor.lock/`, `.context/layout.json`, `.DS_Store` | (a) | layout.json intentionally ignored |
| A15 | `--churn`: ignore each path; `git rm --cached` if tracked | (a)+(b) | Makes editor churn invisible to clean-tree gate |
| A16 | Decision events (once each, via `has_decision`): `log-writers`, `commit-agent`, `doc-agent` | (e) | `append-event.sh decision key=… ref=.context/DECISIONS.md` |
| A17 | `--protect`: `protect-log.sh` → `chflags uappnd` / `chattr +a` on log | (b) | Opt-in OS append-only |
| A18 | If no `agentmon` but `agentsview`: install `templates/agentsview-follow` → `~/.local/bin/agentsview-follow` | (b) | User home, not repo |
| A19 | `--commit`: `git add` scaffold paths; commit `chore: add log-driven workspace scaffold` | (g) git commit | Scaffold list: `.gitignore .githooks/commit-msg .context/bin .context/handoffs .context/DECISIONS.md .context/EVENTLOG.md .context/workspace.env CLAUDE.md AGENTS.md .claude/settings.json` |

### Phase B — `layout.sh` (herdr; requires `HERDR_ENV=1`)

| # | Side effect | Class | Notes |
|---|-------------|-------|-------|
| B1 | Source `.context/workspace.env`; set `MODEL`, `DOC_MODEL` | (g) config read | |
| B2 | Rename **current** workspace → `control`; rename **current** tab → `coordinator` | **(f) adoption** | Invoking pane unchanged; never moved |
| B3 | Eventlog pane: reuse pane labeled `eventlog` in coordinator tab, else `herdr pane split` down 0.5, rename `eventlog`, run `eventlog-view.sh -f` | (c) | Long-running tail |
| B4 | Monitor tab `monitor: system + agents`: run `agentmon --since "$(agent-since.sh)"` or fallback `htop`/`top` | (c) | Skipped with `--no-monitor` |
| B5 | Workspace `maintenance`: create or reuse (label + pane cwd == repo) | (c) shell host | |
| B6 | Tab `lazygit` / pane `gitlog`: run `lazygit` if on PATH | (c) | First tab when workspace is new |
| B7 | Tab `"$MODEL: commit reactor"` / pane `commit-reactor`: if lock pid dead, run `bash .context/bin/run-reactor.sh` | (c) | Supervisor → `cursor-commit-reactor.sh`; skip if `reactor_alive cursor-committer` |
| B8 | Tab `"$DOC_MODEL: doc sync"` / pane `doc-sync`: run `bash .context/bin/run-reactor.sh doc-sync-reactor.sh` | (c) | Skip if `reactor_alive doc-worker` |
| B9 | Wait up to 20s for pane output matching `watching` | (g) readiness gate | Warn only |
| B10 | If no open spawn: `append-event.sh spawn` + `prompt` for `cursor-committer` and `doc-worker` | (e) | `spawn_open`: last spawn seq > last retire seq |
| B11 | Workspace `files`: create/reuse; run `spiceedit` on new workspace | (c) | Skipped with `--no-files` |
| B12 | `herdr tab focus` coordinator tab | (g) UX | |
| B13 | Write `.context/layout.json` (controller/maintenance/committer/doc_worker/files ids) | (b) | Ignored by git; drives stop-hook pane filter + teardown |
| B14 | `layout-map.sh --doctor --workspace "$MAINT"` | (g) validation | Naming standard check; exit 1 on violations |

**Reactor runtime (started in B7/B8, not layout.sh itself):**

| Item | Detail | Class |
|------|--------|-------|
| Lock dir | `.context/events.jsonl.<by>.reactor.lock/` with `pid` file; stale lock reclaimed if pid dead | (b) |
| Supervisor | `run-reactor.sh`: respawn on crash; exit 3 if lock held; crash loop → `escalate` + stop | (c) |
| Resume | First start: baseline `ack outcome=skipped` at log tip; later: max `ack by=<me> seq_done` | (e) internal |
| Committer triggers | `result`, `decision key=commit-message`, `result by=doc-worker` + dirty tree outside `.context/` | (e) consumed |
| Doc triggers | `ack by=cursor-committer outcome=committed` where `origin != doc-worker` | (e) consumed |

**`agent-since.sh` cutoff (feeds B4):**

```bash
# For each herdr pane with .agent != null, earliest foreground process lstart
# cutoff = earliest - AGENT_SINCE_MARGIN (default 60s), RFC3339 UTC
# If none: print "launch"
```

### Phase C — `teardown.sh`

| # | Side effect | Class |
|---|-------------|-------|
| C1 | `kill_tree` on `run-reactor.sh` supervisors and reactor scripts | (g) process kill |
| C2 | `append-event.sh retire agent=cursor-committer\|doc-worker disposition=stopped` if spawn without retire | (e) |
| C3 | `--close`: `herdr workspace close` maintenance + files; `herdr tab close` monitor; `herdr pane close` eventlog; `rm .context/layout.json` | (g) teardown | Controller pane never closed |

### Phase D — `status.sh` (read-only)

No writes. Validates log, hooks, reactors, lock pids, unsanctioned writers, escalations, layout.json pane liveness, agentsview sync.

---

## 3. User-varying parameters

| Parameter | Default | Where set |
|-----------|---------|-----------|
| Committer model | `composer-2.5-fast` | `setup.sh --committer-model`; `workspace.env` `MODEL`; tab label in layout |
| Doc model | `sonnet` | `setup.sh --doc-model`; `workspace.env` `DOC_MODEL` |
| Doc roots | `docs,README.md,AGENTS.md` | `setup.sh --doc-paths`; `workspace.env` `DOC_PATHS`; doc-worker `paths=` in result |
| Pass timeout | 300s commit / 600s doc | `workspace.env` `PASS_TIMEOUT`; reactor env |
| Retries / sleep | 3×20 / 2×30 | Reactor scripts only (`RETRIES`, `RETRY_SLEEP`) |
| Doc budget | `$2` | `DOC_BUDGET_USD` in doc reactor |
| Doc API key | login auth | `DOC_USE_API_KEY=1` keeps `ANTHROPIC_API_KEY` |
| Churn paths | none | `setup.sh --churn` → gitignore + untrack |
| OS protect | off | `setup.sh --protect` |
| Monitor / files workspaces | on | `layout.sh --no-monitor`, `--no-files` |
| Agentmon margin | 60s | `AGENT_SINCE_MARGIN` env |
| Supervisor | pause 5s, max 5 crashes / 120s | `SUPERVISOR_PAUSE`, `SUPERVISOR_MAX_RAPID` |
| Event log path | `.context/events.jsonl` | `EVENTLOG_PATH` |
| Skill paths | `~/.claude/skills/...` | `EVENTLOG_SKILL`, `HERDR_LAYOUTS_SKILL` |
| Herdr ids | from env | `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID` → `layout.json` |

Shell env overrides `workspace.env` (`: "${VAR:=default}"` pattern).

---

## 4. Rows Drovefile cannot express (and why)

| Imperative row | Gap in current Drovefile |
|----------------|--------------------------|
| B2 rename invoking workspace/tab in place | **Missing lifecycle: pane adoption.** Drove creates resources; migration.md §3 says controller pane stays unmanaged. No `adopt_pane` / rename-self. |
| B3 split **existing** controller pane for eventlog | **Missing noun + lifecycle.** `split` applies to declared tab trees at create time, not “split the pane I was launched from.” |
| B10 spawn + prompt events | **Missing noun.** No `event(type=spawn\|prompt\|retire\|decision)` or planner hook after layout. |
| A16 decision events at setup | Same; bootstrap runs shell, does not append coordination events. |
| B7/B8 reactor_alive skip + stale lock reclaim | **Wrong lifecycle.** `command` assumes (re)start; no “start only if lock pid dead” or lock-dir contract. |
| B9 wait for `watching` | **Missing readiness gate** between pane run and success. |
| B4 dynamic `agentmon --since "$(agent-since.sh)"` | **No eval/exec in Drovefile** (by design); needs precomputed argv or a wrapper script + static command. |
| Conditional commands (agentmon vs htop, lazygit vs shell, spiceedit vs shell) | **Missing conditional pane command** (tool presence). |
| Tab labels embedding `$MODEL` / `$DOC_MODEL` | Labels are static strings; no interpolation from profile vars in drovefile.md. |
| A12 never regenerate `workspace.env` | **Missing policy:** “write once, never reconcile.” Bootstrap always check/run; no “create-if-absent-only.” |
| A13 never regenerate `DECISIONS.md` | Same. |
| A8 keep AGENTS block unless `--force` | Bootstrap idempotency is check/run, not template splice with preserve semantics. |
| C2 retire on teardown | **Missing teardown profile** or `down` lifecycle events. |
| C3 `--close` selective close preserving controller | **Missing partial teardown** (close declared children, never adoptor). |
| B13 `.context/layout.json` | No first-class machine-local state file (ignored path). Stop hook reads it for pane identity. |
| A6 `core.hooksPath` | Expressible as `bootstrap` **if** repo wraps check/run scripts listing inputs. |
| A10 Stop hook, A2 PreToolUse guard | Expressible as bootstrap tasks; not in default Drovefile vocabulary. |
| A15 churn untrack | Bootstrap could run it; no dedicated `untrack` noun. |
| A19 `--commit` scaffold commit | Out of scope for Drove reconcile (one-shot human/agent commit). |
| Reactor ack/note/escalate/violation events | **Missing** — runtime behavior stays in reactor scripts (migration.md §4 agrees). |

---

## 5. Reuse / idempotency contract (`layout.sh`) — rules for Drove planner

1. **Controller adoption:** The invoking herdr pane is immutable identity for the controller; only rename workspace→`control`, tab→`coordinator`. Never create a replacement controller tab.

2. **Workspace reuse:** A workspace matches if `label` equals and **some pane** has `cwd` or `foreground_cwd` == repo root (`find_workspace`).

3. **Tab reuse:** Within a workspace, tab matches if `label` equals and **some pane in that tab** has cwd/foreground_cwd == repo (`find_tab`). Reuse returns existing tab_id + pane_id; **does not re-run command** unless reactor restart rule applies.

4. **Pane reuse by label:** Eventlog: pane labeled `eventlog` in coordinator tab → skip split.

5. **Reactor reuse:** If `.context/events.jsonl.<agent>.reactor.lock/pid` exists and `kill -0 pid` → do not start second supervisor. Dead pid → reclaim lock and start in **existing** pane via `herdr pane run`.

6. **Spawn/prompt idempotency:** Append spawn+prompt only when `spawn_open(agent)`: latest `spawn` seq for agent > latest `retire` seq (or no retire).

7. **Maintenance workspace ordering:** On **new** maintenance workspace, root tab becomes `lazygit` first; reactor tabs added after. On reuse, `ensure_tab` for lazygit then reactors.

8. **Optional surfaces:** `--no-monitor` / `--no-files` skip creation entirely; second run with flags does not remove existing (no teardown in layout).

9. **Focus:** Always refocus coordinator tab after layout.

10. **Persisted map:** Write `.context/layout.json` every successful run for stop-hook pane scoping and teardown `--close`.

11. **Second full run:** Prints `reuse` everywhere; no duplicate tabs, reactors, or open spawns.

12. **Dry-run:** `--dry-run` prints herdr commands, returns `{}` for jq ids; no HERDR_ENV required.

**Drove must match or beat:** discovery before create (label + repo cwd), reactor lock-aware start, spawn dedup, adopt-not-recreate controller, and selective teardown that never kills the adoptor pane.

---

## JSON shapes (coordination events layout writes)

```json
{"seq":N,"ts":"…","type":"spawn","agent":"cursor-committer","model":"…","tab":"…","pane":"…","role":"commit-reactor","runtime":"cursor-headless"}
{"seq":N,"ts":"…","type":"prompt","agent":"cursor-committer","ref":".context/handoffs/cursor-committer.md"}
{"seq":N,"ts":"…","type":"retire","agent":"cursor-committer","disposition":"stopped","detail":"teardown.sh"}
```

`layout.json` top-level keys: `controller`, `maintenance`, `committer`, `doc_worker`, `files` (each holds workspace/tab/pane ids and models).
