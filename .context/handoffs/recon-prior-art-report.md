# Prior-art recon: terminal workspaces → Drove Starlark DSL

Web search + official docs (Sep 2026). Drove baseline: `docs/drovefile.md` — `profile` → `workspace` → `tab` → binary `split()` of `pane(id, label, command=[...])`; separate `agent`, `bootstrap(check/run, depends_on)`.

---

## 1. Per-tool summaries and ideas

### Tilt (Tiltfile)

Starlark declares **named resources** (local/k8s). Reconciler watches files, orders via `resource_deps`, gates on probes, UI for enable/disable/trigger. Flat resource list; UI `labels` group only.

```python
local_resource("db", serve_cmd="postgres", readiness_probe=probe(tcp_socket=tcp_socket_action(port=5432)))
local_resource("api", serve_cmd="cargo run", resource_deps=["db"])
local_resource("lint", cmd="make lint", auto_init=False, trigger_mode=TRIGGER_MODE_AUTO, deps=["src/"])
config.define_string_list("to-run", args=True); cfg = config.parse(); config.set_enabled_resources(cfg.get("to-run", []))
load('ext://uibutton', 'cmd_button'); cmd_button('migrate', argv=['make', 'migrate'], resource='api')
watch_file('k8s/base.yaml')
```

---

### process-compose

YAML `processes:` map. `depends_on` + **conditions**, readiness/liveness probes, restart policy. TUI supervisor; deps are **startup-order only**.

```yaml
depends_on:
  postgres: { condition: process_healthy }
  migrate: { condition: process_completed_successfully }
readiness_probe:
  http_get: { host: localhost, port: 8000, path: /health }
  period_seconds: 10
migrate:
  command: alembic upgrade head
  availability: { restart: exit_on_failure }
```

---

### Overmind / Procfile (foreman)

Flat `name: command` lines. Overmind → tmux windows, per-process restart/connect. No deps, no layout. foreman adds `-m web=2` formation, `-e .env`.

```
web: bundle exec rails s
worker: bundle exec sidekiq
# .overmind.env
OVERMIND_CAN_DIE=migrate,assets
OVERMIND_PROCFILE=Procfile.dev
OVERMIND_PROCESSES=web,worker
```

---

### mprocs

YAML `procs:` dict; fixed list+log UI. `cmd` vs `shell`, `autostart`/`autorestart`, global+local config merge.

```yaml
procs:
  server: { cmd: ["cargo", "run"], autostart: true }
  tests: { shell: "jest -w", autostart: false, autorestart: true }
# ~/.config/mprocs/mprocs.yaml merged with ./mprocs.yaml
```

---

### Zellij (layout KDL)

Nested KDL tree: tabs/panes, splits, cwd, command. **Templates** with `children`; **swap layouts** reflow by pane count.

```kdl
tab_template name="dev" { children; pane size=1 { plugin location="zellij:status-bar" } }
tab name="api" cwd="./server" focus=true { pane command="cargo run" }
swap_tiled_layout name="vertical" {
  ui max_panes=3 { pane; pane; pane; }
  ui max_panes=8 { pane split_direction="vertical" { pane { children; } pane { pane; pane; } } }
}
```

---

### tmuxinator

YAML: `root`, `windows[]` with tmux layout preset + `panes`. Generates tmux script. `startup_window`, `focused_pane`, `pre_window`.

```yaml
root: ~/app
startup_window: editor
windows:
  - editor:
      layout: main-vertical
      focused_pane: editor
      panes: [editor: vim, guard]
  - server: bundle exec rails s
```

---

### tmuxp

tmuxinator-like + **`before_script`** (pre-session, exit-code gated) + **`shell_command_before`** (every pane).

```yaml
before_script: ./bootstrap.sh
windows:
  - main:
      shell_command_before: [source .venv/bin/activate]
      start_directory: doc/
      panes:
        - shell_command: [npm, start]
```

---

### devcontainer.json

JSON lifecycle chain, features, port forward metadata. Merge across features + user file.

```json
{
  "features": { "ghcr.io/devcontainers/features/github-cli": {} },
  "onCreateCommand": ["npm", "ci"],
  "postCreateCommand": { "server": "npm start", "db": ["mysql", "-u", "root"] },
  "waitFor": "onCreateCommand",
  "forwardPorts": [3000, "db:5432"]
}
```

---

### Docker Compose

`services:` map; `depends_on.condition`, `healthcheck`, `profiles`, `extends`. Manages labeled containers only.

```yaml
services:
  web:
    depends_on:
      db: { condition: service_healthy }
      migrate: { condition: service_completed_successfully }
  db:
    healthcheck: { test: ["CMD-SHELL", "pg_isready"], interval: 10s, retries: 5 }
  debug: { profiles: [debug], image: debug-tools }
  web: { extends: { file: common.yml, service: web-base } }
```

---

### Bazel / Starlark macros

`.bzl` macros: implicit **`name`**, `attrs`, **`**kwargs`** forward, **`select()`** variants, `load()`.

```python
def dev_pane(name, command, **kwargs):
    return pane(id=name, label=kwargs.pop("label", name), command=command, **kwargs)
command = select({"//config:macos": ["./run-mac.sh"], "default": ["./run.sh"]})
```

---

### Pulumi / CDK components

Component groups children; `parent:` inheritance; prefixed child names; `registerOutputs`.

```python
def coordinator_stack(name, model="composer-2.5-fast"):
    p = pane(id=name, adopt=True)
    a = agent(id=name, pane=name, kind="cursor", args=["--model", model])
    return struct(pane=p, agent=a)
# CDK: new MyConstruct(scope, "Web", { ... }) — same parent/child pattern
```

---

## 2. Cross-cutting matrix

| | Naming | Layout | Long vs one-shot | Readiness/deps | Reuse | Profiles | Host override | Adoption |
|--|--------|--------|------------------|----------------|-------|----------|---------------|----------|
| **Tilt** | string name; labels | flat | cmd/serve_cmd; trigger_mode | resource_deps + probe | load/ext | config.define_* | tilt_config.json | disable; down-policy:keep |
| **process-compose** | process key | flat | availability/exit_on_end | condition + probe | copy yaml | namespace | env in yaml | attach TUI |
| **Overmind** | proc name | flat | CAN_DIE | none | shared Procfile | env subset | .overmind.env | overmind connect |
| **mprocs** | proc key | fixed UI | autorestart | none | global yaml | autostart | global+local | in-place restart |
| **Zellij** | optional name | **tree+template+swap** | pane command | none | templates | layout files | user config | load into session |
| **tmuxinator/tmuxp** | window name | tree+preset | pane cmds | before_script order | copy yaml | multi project | ~/.tmuxinator | tmux attach |
| **devcontainer** | feature keys | n/a | lifecycle hooks | waitFor chain | features | feature toggles | merge | reuse container |
| **Compose** | service name | flat | restart policy | condition+healthcheck | extends | profiles | override files | --no-recreate; ext network |
| **Bazel** | name attr | call graph | rules vs genrule | deps attr | load/macros | select() | .bazelrc | N/A |
| **Pulumi/CDK** | component name | hierarchy | N/A | parent graph | components | stack config | config yaml | import/retain |

---

## 3. Top 10 ideas → Drovefile sketches

### 1. Flat `resource()` + placement hint (Tilt)
```python
resource(id="api", serve=["cargo","run"], cwd="server",
         depends_on=["postgres"], ready=tcp_probe(8080),
         place=pane_in(tab="main", slot="right"))
```

### 2. Typed depends_on conditions (process-compose/Compose)
```python
resource(id="web", serve=["npm","start"],
         depends_on={"migrate": completed_ok(), "postgres": healthy()})
```

### 3. serve vs run lifecycle (Tilt)
```python
task(id="seed", run=["./scripts/seed"], once=True)
pane(id="api", serve=["cargo","run"])
```

### 4. Readiness probes (Tilt/process-compose)
```python
ready = probe(http={"port":3000,"path":"/health"}, initial_delay="5s")
resource(id="frontend", serve=["npm","start"], ready=ready)
```

### 5. Template + children slot (Zellij/Pulumi)
```python
def control_tab(children):
    return tab(id="control", layout=split("down", 0.2,
        pane(id="status"), children))
```

### 6. config.define_* enable lists (Tilt)
```python
config.define_string_list("workspaces", args=True)
cfg = config.parse()
profile(name="default", workspaces=cfg.get("workspaces", ["development"]))
```

### 7. adopt invoking pane (Overmind connect + Drove need)
```python
pane(id="coordinator", adopt=True)  # reconcile focus/id; no new exec
agent(id="coordinator", pane="coordinator", kind="cursor")
```

### 8. Macro with implicit name (Bazel)
```python
load("drove/agents.star", "cursor_agent")
cursor_agent("review", model="composer-2.5-fast")  # pane+agent
```

### 9. Parallel bootstrap object (devcontainer)
```python
bootstrap(id="setup", parallel={
    "hooks": ["./scripts/install-hooks"],
    "direnv": ["direnv","allow"],
})
```

### 10. Layout variants by pane count (Zellij swap)
```python
layout_variant(name="many-agents", when_panes_gte=5, layout=grid(cols=3))
```

---

## 4. Anti-patterns Drove should avoid

- **Startup deps fire once** (Tilt): re-check readiness on reconcile or document semantics.
- **depends_on implies crash coupling** (Compose/process-compose): keep start-order separate from supervision.
- **Shell-only Procfile lines**: keep argv arrays; optional gated `shell=`.
- **id/label duplication** (Drove, tmuxinator): default label=id.
- **Binary splits for lists** (Drove): offer `row([...])` or placement hints.
- **Simultaneous start races** (Overmind/tmux): require probes or log-line ready.
- **Profile = duplicate files**: subset via config (Tilt/mprocs/Compose profiles).
- **Secrets in repo yaml** (process-compose): env pass-through + gitignored overlay.
- **Fake adoption of foreign PIDs** (Compose vs docker run): explicit `adopt`/external ref.
- **Disable ignores dep closure** (Tilt UI): warn or cascade.
- **Unsandboxed Tiltfile power**: keep Drove eval deterministic (already in spec).
- **Copy-paste yaml** without templates: macros + `children` injection.
- **Opaque config merge** (devcontainer/mprocs): document precedence.
