//! Restricted Starlark evaluation for repository-owned `Drovefile`s.
#![expect(
    unsafe_code,
    reason = "starlark derives its runtime type marker as an unsafe trait"
)]

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde_json::Value as JsonValue;
use starlark::{
    any::ProvidesStaticType,
    environment::{FrozenModule, Globals, GlobalsBuilder, LibraryExtension, Module},
    eval::{Evaluator, FileLoader},
    starlark_module,
    syntax::{AstModule, Dialect},
    values::{FrozenHeapName, Value, none::NoneType},
};

use crate::model::{BackendTargets, DroveConfig, Profile, canonical_digest};

const PRELUDE: &str = r#"
def _compact(values):
    return {k: v for (k, v) in values.items() if v != None and v != [] and v != {}}

def _serve_candidates(serve):
    if serve == None or len(serve) == 0:
        return []
    if type(serve[0]) == "string":
        return [serve]
    return serve

def any_of(*argvs):
    return list(argvs)

def output(text):
    return {"kind": "output", "value": text}

def port(n):
    return {"kind": "port", "value": n}

def cmd(argv):
    return {"kind": "cmd", "value": argv}

def file(path):
    return {"kind": "file", "path": path}

def agent(kind, args = [], prompt = None, name = None):
    return _compact({
        "kind": kind,
        "args": args,
        "prompt": prompt,
        "name": name,
    })

def pane(name, label = None, cwd = None, env = {}, serve = None, ready = None,
         after = [], adopt = None, agent = None, on_start = None, on_stop = None):
    return _compact({
        "name": name,
        "label": label,
        "cwd": cwd,
        "env": env,
        "serve": _serve_candidates(serve),
        "ready": ready,
        "after": after,
        "adopt": adopt,
        "agent": agent,
        "on_start": on_start,
        "on_stop": on_stop,
    })

def tab(name, label = None, panes = [], split = "right", ratios = []):
    return _compact({
        "name": name,
        "label": label,
        "panes": panes,
        "split": split,
        "ratios": ratios,
    })

def workspace(name, label = None, cwd = ".", env = {}, tabs = []):
    return _compact({
        "name": name,
        "label": label,
        "cwd": cwd,
        "env": env,
        "tabs": tabs,
    })

def task(name, run = [], check = None, inputs = [], after = [], auto = True,
         on_start = None, on_stop = None):
    return _compact({
        "name": name,
        "run": run,
        "check": check,
        "inputs": inputs,
        "after": after,
        "auto": auto,
        "on_start": on_start,
        "on_stop": on_stop,
    })

def profile(name, workspaces = [], tasks = [], extends = None, without = []):
    return _emit_profile(_compact({
        "name": name,
        "workspaces": workspaces,
        "tasks": tasks,
        "extends": extends,
        "without": without,
    }))

def backend(id):
    return _set_backend(id)

def _herdr_session(name):
    return _set_herdr_session(name)

herdr = struct(
    session = _herdr_session,
)

def _radiator_hub(name):
    return _set_radiator_hub(name)

radiator = struct(
    hub = _radiator_hub,
)
"#;

#[derive(Debug)]
pub struct CompiledDrovefile {
    pub config: DroveConfig,
    pub repo_root: PathBuf,
    pub source_digest: String,
}

#[derive(Debug, ProvidesStaticType, Default)]
struct ProfileStore {
    profiles: RefCell<Vec<JsonValue>>,
    /// `backend(...)` (D32): which backend this project reconciles onto.
    backend: RefCell<Option<String>>,
    /// `herdr.session(...)` (D32).
    herdr_session: RefCell<Option<String>>,
    /// `radiator.hub(...)` (D32).
    radiator_hub: RefCell<Option<String>>,
}

#[starlark_module]
fn drove_globals(builder: &mut GlobalsBuilder) {
    fn _emit_profile<'v>(
        value: Value<'v>,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<Value<'v>> {
        let store = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<ProfileStore>())
            .context("Drove evaluator profile store is missing")?;
        store.profiles.borrow_mut().push(value.to_json_value()?);
        Ok(value)
    }

    fn _set_backend<'v>(id: String, eval: &mut Evaluator<'v, '_, '_>) -> anyhow::Result<NoneType> {
        let store = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<ProfileStore>())
            .context("Drove evaluator profile store is missing")?;
        *store.backend.borrow_mut() = Some(id);
        Ok(NoneType)
    }

    fn _set_herdr_session<'v>(
        name: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let store = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<ProfileStore>())
            .context("Drove evaluator profile store is missing")?;
        *store.herdr_session.borrow_mut() = Some(name);
        Ok(NoneType)
    }

    fn _set_radiator_hub<'v>(
        name: String,
        eval: &mut Evaluator<'v, '_, '_>,
    ) -> anyhow::Result<NoneType> {
        let store = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<ProfileStore>())
            .context("Drove evaluator profile store is missing")?;
        *store.radiator_hub.borrow_mut() = Some(name);
        Ok(NoneType)
    }
}

#[derive(Default)]
struct RepositoryLoader {
    modules: BTreeMap<String, FrozenModule>,
}

impl FileLoader for RepositoryLoader {
    fn load(&self, path: &str) -> starlark::Result<FrozenModule> {
        self.modules.get(path).cloned().ok_or_else(|| {
            starlark::Error::new_other(anyhow::anyhow!(
                "load path `{path}` was not compiled from this repository"
            ))
        })
    }
}

pub fn compile(path: &Path) -> Result<CompiledDrovefile> {
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", path.display()))?;
    let repo_root = path
        .parent()
        .context("Drovefile must have a parent directory")?
        .canonicalize()
        .context("cannot resolve repository root")?;
    let main_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Drovefile name must be UTF-8")?
        .to_owned();

    let globals = GlobalsBuilder::extended_by(&[LibraryExtension::StructType])
        .with(drove_globals)
        .build();
    let store = ProfileStore::default();
    let mut loader = RepositoryLoader::default();
    let mut sources = BTreeMap::from([("<drove-prelude>".to_owned(), PRELUDE.to_owned())]);
    let mut stack = Vec::new();
    compile_module(
        &main_id,
        &repo_root,
        &globals,
        &store,
        &mut loader,
        &mut sources,
        &mut stack,
    )?;

    let backend = store.backend.into_inner();
    let target = BackendTargets {
        herdr_session: store.herdr_session.into_inner(),
        radiator_hub: store.radiator_hub.into_inner(),
    };
    let raw_profiles = store.profiles.into_inner();
    if raw_profiles.is_empty() {
        bail!("Drovefile did not declare any profiles");
    }
    let profiles = resolve_profiles(raw_profiles, &repo_root)?;

    Ok(CompiledDrovefile {
        config: DroveConfig::new(profiles, backend, target)?,
        repo_root,
        source_digest: canonical_digest(&sources)?,
    })
}

/// Resolves `extends`/`without` composition and `file()` prompt references
/// against already-emitted profiles, in declaration order, entirely in Rust
/// after Starlark evaluation has finished (the sandbox never touches the
/// filesystem or other profiles during evaluation itself).
fn resolve_profiles(raw_profiles: Vec<JsonValue>, repo_root: &Path) -> Result<Vec<Profile>> {
    let mut resolved: BTreeMap<String, (Vec<JsonValue>, Vec<JsonValue>)> = BTreeMap::new();
    let mut profiles = Vec::with_capacity(raw_profiles.len());

    for mut raw in raw_profiles {
        resolve_file_prompts(&mut raw, repo_root)?;
        let name = raw
            .get("name")
            .and_then(JsonValue::as_str)
            .context("profile declaration is missing `name`")?
            .to_owned();
        let mut workspaces = raw
            .get("workspaces")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();
        let mut tasks = raw
            .get("tasks")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();

        if let Some(extends) = raw.get("extends").and_then(JsonValue::as_str) {
            let (base_workspaces, base_tasks) = resolved
                .get(extends)
                .with_context(|| format!("profile `{name}` extends unknown profile `{extends}`"))?;
            let mut inherited_workspaces = base_workspaces.clone();
            inherited_workspaces.append(&mut workspaces);
            workspaces = inherited_workspaces;
            let mut inherited_tasks = base_tasks.clone();
            inherited_tasks.append(&mut tasks);
            tasks = inherited_tasks;
        }

        if let Some(without) = raw.get("without").and_then(JsonValue::as_array) {
            let excluded: Vec<&str> = without.iter().filter_map(JsonValue::as_str).collect();
            for name_to_drop in &excluded {
                if !workspaces.iter().any(|workspace| {
                    workspace.get("name").and_then(JsonValue::as_str) == Some(*name_to_drop)
                }) {
                    bail!(
                        "profile `{name}` declares without = [\"{name_to_drop}\"] for an unknown workspace"
                    );
                }
            }
            if !excluded.is_empty() {
                workspaces.retain(|workspace| {
                    let workspace_name = workspace.get("name").and_then(JsonValue::as_str);
                    !workspace_name.is_some_and(|name| excluded.contains(&name))
                });
            }
        }

        resolved.insert(name.clone(), (workspaces.clone(), tasks.clone()));
        profiles.push(serde_json::json!({
            "name": name,
            "workspaces": workspaces,
            "tasks": tasks,
        }));
    }

    profiles
        .into_iter()
        .map(serde_json::from_value::<Profile>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("invalid profile declaration")
}

/// Recursively resolves `prompt = file("repo/relative/path")` into the file's
/// text content (D25), validating the path stays inside the repository.
fn resolve_file_prompts(value: &mut JsonValue, repo_root: &Path) -> Result<()> {
    match value {
        JsonValue::Object(map) => {
            if let Some(prompt) = map.get("prompt")
                && let Some(file_path) = file_reference_path(prompt)
            {
                let text = read_repo_file(repo_root, file_path)?;
                map.insert("prompt".to_owned(), JsonValue::String(text));
            }
            for child in map.values_mut() {
                resolve_file_prompts(child, repo_root)?;
            }
        }
        JsonValue::Array(items) => {
            for item in items.iter_mut() {
                resolve_file_prompts(item, repo_root)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn file_reference_path(value: &JsonValue) -> Option<&str> {
    let object = value.as_object()?;
    if object.get("kind").and_then(JsonValue::as_str) != Some("file") {
        return None;
    }
    object.get("path").and_then(JsonValue::as_str)
}

fn read_repo_file(repo_root: &Path, relative: &str) -> Result<String> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("file(\"{relative}\") must stay inside the repository");
    }
    let path = repo_root.join(relative_path);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("cannot resolve file(\"{relative}\")"))?;
    if !canonical.starts_with(repo_root) {
        bail!("file(\"{relative}\") escapes the repository");
    }
    fs::read_to_string(&canonical).with_context(|| format!("cannot read file(\"{relative}\")"))
}

fn compile_module(
    module_id: &str,
    repo_root: &Path,
    globals: &Globals,
    store: &ProfileStore,
    loader: &mut RepositoryLoader,
    sources: &mut BTreeMap<String, String>,
    stack: &mut Vec<String>,
) -> Result<FrozenModule> {
    if let Some(module) = loader.modules.get(module_id) {
        return Ok(module.clone());
    }
    if stack.iter().any(|item| item == module_id) {
        bail!(
            "cyclic Starlark load: {} -> {module_id}",
            stack.join(" -> ")
        );
    }

    let relative = Path::new(module_id);
    if relative.is_absolute() {
        bail!("absolute Starlark load paths are not allowed: `{module_id}`");
    }
    let path = repo_root.join(relative);
    let canonical = path
        .canonicalize()
        .with_context(|| format!("cannot load `{module_id}`"))?;
    if !canonical.starts_with(repo_root) {
        bail!("Starlark load escapes repository root: `{module_id}`");
    }

    let source =
        fs::read_to_string(&canonical).with_context(|| format!("cannot read `{module_id}`"))?;
    sources.insert(module_id.to_owned(), source.clone());
    let ast = AstModule::parse(
        module_id,
        format!("{PRELUDE}\n{source}"),
        &Dialect::Standard,
    )
    .map_err(|error| anyhow::anyhow!("cannot parse `{module_id}`: {error}"))?;

    stack.push(module_id.to_owned());
    let load_ids = ast
        .loads()
        .into_iter()
        .map(|load| load.module_id.to_owned())
        .collect::<Vec<_>>();
    for load_id in load_ids {
        compile_module(&load_id, repo_root, globals, store, loader, sources, stack)?;
    }
    stack.pop();

    let module = Module::with_temp_heap(|module| -> Result<FrozenModule> {
        let mut evaluator = Evaluator::new(&module);
        evaluator.extra = Some(store);
        evaluator.set_loader(loader);
        evaluator.set_max_callstack_size(256)?;
        evaluator.set_max_heap_size(64 * 1024 * 1024)?;
        evaluator.set_max_tick_count(1_000_000)?;
        evaluator
            .eval_module(ast, globals)
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        drop(evaluator);
        module
            .freeze_named(FrozenHeapName::User(Box::new(module_id.to_owned())))
            .map_err(|error| anyhow::anyhow!("{error:?}"))
    })
    .with_context(|| format!("cannot evaluate `{module_id}`"))?;
    loader.modules.insert(module_id.to_owned(), module.clone());
    Ok(module)
}

pub fn find_drovefile(start: &Path) -> Result<PathBuf> {
    let mut directory = start
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", start.display()))?;
    if directory.is_file() {
        directory.pop();
    }
    loop {
        let candidate = directory.join("Drovefile");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !directory.pop() {
            bail!("no Drovefile found from {}", start.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn compiles_profile_and_rejects_escaping_loads() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
profile(
    name = "default",
    workspaces = [
        workspace(
            name = "dev",
            tabs = [tab(name = "main", panes = [pane(name = "shell")])],
        ),
    ],
)
"#,
        )
        .expect("write fixture");

        let compiled = compile(&directory.path().join("Drovefile")).expect("compile");
        assert_eq!(
            compiled
                .config
                .profile("default")
                .expect("profile")
                .workspaces
                .len(),
            1
        );

        fs::write(
            directory.path().join("Drovefile"),
            "load(\"../outside.star\", \"x\")\nprofile(name = \"default\")",
        )
        .expect("write fixture");
        let error = compile(&directory.path().join("Drovefile")).expect_err("escape should fail");
        assert!(
            error.to_string().contains("cannot load")
                || error.to_string().contains("escapes repository")
        );
    }

    #[test]
    fn backend_and_radiator_hub_round_trip_into_the_compiled_config() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
backend("radiator")
radiator.hub("main")
herdr.session("drove")
profile(name = "default")
"#,
        )
        .expect("write fixture");

        let compiled = compile(&directory.path().join("Drovefile")).expect("compile");
        assert_eq!(compiled.config.backend.as_deref(), Some("radiator"));
        assert_eq!(compiled.config.target.radiator_hub.as_deref(), Some("main"));
        assert_eq!(
            compiled.config.target.herdr_session.as_deref(),
            Some("drove")
        );
    }

    #[test]
    fn repository_local_loads_affect_the_source_digest() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("layout.star"),
            "main_tab = tab(name = \"main\", panes = [pane(name = \"shell\")])",
        )
        .expect("write helper");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
load("layout.star", "main_tab")
profile(
    name = "default",
    workspaces = [workspace(name = "dev", tabs = [main_tab])],
)
"#,
        )
        .expect("write Drovefile");

        let first = compile(&directory.path().join("Drovefile")).expect("compile");
        fs::write(
            directory.path().join("layout.star"),
            "main_tab = tab(name = \"main\", label = \"changed\", panes = [pane(name = \"shell\")])",
        )
        .expect("change helper");
        let second = compile(&directory.path().join("Drovefile")).expect("recompile");
        assert_ne!(first.source_digest, second.source_digest);
        assert_eq!(
            second.config.profiles["default"].workspaces[0].tabs[0].label(),
            "changed"
        );
    }

    #[test]
    fn profile_extends_and_without_compose_workspaces() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
default_ws = workspace(name = "control", tabs = [tab(name = "main", panes = [pane(name = "shell")])])
files_ws = workspace(name = "files", tabs = [tab(name = "files", panes = [pane(name = "editor")])])

profile(name = "default", workspaces = [default_ws, files_ws])
profile(name = "core", extends = "default", without = ["files"])
"#,
        )
        .expect("write fixture");

        let compiled = compile(&directory.path().join("Drovefile")).expect("compile");
        let core = compiled.config.profile("core").expect("core profile");
        assert_eq!(core.workspaces.len(), 1);
        assert_eq!(core.workspaces[0].name, "control");
    }

    #[test]
    fn profile_extends_composes_child_workspaces_with_inherited_ones() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
control_ws = workspace(name = "control", tabs = [tab(name = "main", panes = [pane(name = "shell")])])
extra_ws = workspace(name = "extra", tabs = [tab(name = "extra", panes = [pane(name = "editor")])])

profile(name = "default", workspaces = [control_ws])
profile(name = "core", extends = "default", workspaces = [extra_ws])
"#,
        )
        .expect("write fixture");

        let compiled = compile(&directory.path().join("Drovefile")).expect("compile");
        let core = compiled.config.profile("core").expect("core profile");
        let names: Vec<&str> = core
            .workspaces
            .iter()
            .map(|workspace| workspace.name.as_str())
            .collect();
        assert_eq!(names, ["control", "extra"]);
    }

    #[test]
    fn without_an_unknown_workspace_fails() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
default_ws = workspace(name = "control", tabs = [tab(name = "main", panes = [pane(name = "shell")])])

profile(name = "default", workspaces = [default_ws])
profile(name = "core", extends = "default", without = ["typo"])
"#,
        )
        .expect("write fixture");

        let error = compile(&directory.path().join("Drovefile")).expect_err("unknown without");
        assert!(error.to_string().contains("unknown workspace"));
    }

    #[test]
    fn profile_extends_unknown_profile_fails() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            "profile(name = \"default\")\nprofile(name = \"core\", extends = \"missing\")",
        )
        .expect("write fixture");
        let error = compile(&directory.path().join("Drovefile")).expect_err("unknown extends");
        assert!(error.to_string().contains("extends unknown profile"));
    }

    #[test]
    fn profile_returns_the_value_it_registers() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
result = profile(name = "default")
_check = result["name"]
"#,
        )
        .expect("write fixture");
        compile(&directory.path().join("Drovefile")).expect("compile");
    }

    #[test]
    fn resolves_file_prompt_at_compile_time() {
        let directory = tempdir().expect("tempdir");
        fs::write(directory.path().join("prompt.md"), "Read the brief.").expect("write prompt");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review", agent = agent("claude", prompt = file("prompt.md"))),
        ])]),
    ],
)
"#,
        )
        .expect("write fixture");

        let compiled = compile(&directory.path().join("Drovefile")).expect("compile");
        let profile = compiled.config.profile("default").expect("profile");
        let agent = profile.workspaces[0].tabs[0].panes[0]
            .agent
            .as_ref()
            .expect("agent");
        assert_eq!(agent.prompt.as_deref(), Some("Read the brief."));
    }

    #[test]
    fn file_prompt_cannot_escape_repository() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
profile(
    name = "default",
    workspaces = [
        workspace(name = "dev", tabs = [tab(name = "main", panes = [
            pane(name = "review", agent = agent("claude", prompt = file("../outside.md"))),
        ])]),
    ],
)
"#,
        )
        .expect("write fixture");
        let error = compile(&directory.path().join("Drovefile")).expect_err("escape");
        assert!(error.to_string().contains("repository") || error.to_string().contains("resolve"));
    }
}
