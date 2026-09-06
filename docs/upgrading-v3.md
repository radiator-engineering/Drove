# Upgrading a Drovefile from v2 to v3

Drove v3 restructures the DSL around a core every backend honors, plus per-backend flavors (`docs/spec.md`, "Core and Flavors"). Herdr's tab and split vocabulary moves out of the core constructors and into a `herdr` namespace. Every v2 form below still compiles for one release, so an existing Drovefile keeps working; it prints a warning naming the exact rewrite.

## The shims

| v2 form | v3 rewrite |
| --- | --- |
| `pane(name, adopt = "caller")` | `caller_pane(name, ...)` |
| `tab(name, ...)` | `herdr.tab(name, ...)` |
| `workspace(name, tabs = [...])` | `workspace(name, panes = [herdr.tab(...)])` |
| `split = "right"` / `"down"` | `split = herdr.RIGHT` / `herdr.DOWN` |

Each shim compiles to the same v3 result the rewrite would produce, and warns once per occurrence. `drove render` and `drove plan` print every warning collected while compiling the file.

These shims last one release. After that, the v2 forms are removed and a Drovefile that still uses them fails to compile.

## No restarts from upgrading alone

A pane's content digest — `serve`, `cwd`, `env`, `agent`, `ready`, `on_start`, `on_stop` — excludes its placement. Rewriting `tab(...)` to `herdr.tab(...)`, or `workspace(tabs = [...])` to `workspace(panes = [...])`, changes only placement, so an upgraded Drovefile produces the same content digest for every pane it already declared. `drove up` proposes no restarts from the rewrite by itself. Moving a pane to a different tab does change its topology digest and is a real, confirmed replace — that's a layout change, not a syntax upgrade.

## How to migrate

1. Run `drove render` against your current Drovefile. If it uses any v2 form, the warnings list each one and the output ends with a `v3 form:` block: your file, rewritten.
2. Copy that block over your Drovefile, or apply the rewrites from the table above by hand.
3. Run `drove render` again. A clean v3 file compiles with no warnings.
4. Run `drove plan`. It should propose no changes beyond what you'd expect from the upgrade being purely syntactic.
5. Run `drove lint` to catch a stale `was = "..."` or a task with no `check` while you're already touching the file.

See `docs/drovefile.md` for the full v3 reference and `docs/spec.md` for why the core/flavor split exists.
