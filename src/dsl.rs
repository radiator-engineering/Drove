//! Restricted Starlark evaluation for repository-owned `Drovefile`s.
#![expect(
    unsafe_code,
    reason = "starlark derives its runtime type marker as an unsafe trait"
)]

use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use starlark::{
    any::ProvidesStaticType,
    environment::{FrozenModule, Globals, GlobalsBuilder, Module},
    eval::{Evaluator, FileLoader},
    starlark_module,
    syntax::{AstModule, Dialect},
    values::{FrozenHeapName, Value, none::NoneType},
};

use crate::model::{DroveConfig, Profile, canonical_digest};

const PRELUDE: &str = r#"
def _compact(values):
    return {k: v for (k, v) in values.items() if v != None and v != [] and v != {}}

def pane(id, label = None, cwd = None, command = [], env = {}):
    return _compact({
        "type": "pane",
        "id": id,
        "label": label,
        "cwd": cwd,
        "command": command,
        "env": env,
    })

def split(direction, ratio, first, second):
    return {
        "type": "split",
        "direction": direction,
        "ratio": ratio,
        "first": first,
        "second": second,
    }

def tab(id, label, layout):
    return {"id": id, "label": label, "layout": layout}

def workspace(id, label, tabs, cwd = "."):
    return {"id": id, "label": label, "cwd": cwd, "tabs": tabs}

def agent(id, pane, kind, name = None, args = []):
    return _compact({
        "id": id,
        "pane": pane,
        "kind": kind,
        "name": name,
        "args": args,
    })

def bootstrap(id, check, run, inputs = [], depends_on = []):
    return {
        "id": id,
        "check": check,
        "run": run,
        "inputs": inputs,
        "depends_on": depends_on,
    }

def profile(name, workspaces = [], agents = [], bootstrap = []):
    _emit_profile({
        "name": name,
        "workspaces": workspaces,
        "agents": agents,
        "bootstrap": bootstrap,
    })
"#;

#[derive(Debug)]
pub struct CompiledDrovefile {
    pub config: DroveConfig,
    pub repo_root: PathBuf,
    pub source_digest: String,
}

#[derive(Debug, ProvidesStaticType, Default)]
struct ProfileStore(RefCell<Vec<serde_json::Value>>);

#[starlark_module]
fn drove_globals(builder: &mut GlobalsBuilder) {
    fn _emit_profile(value: Value, eval: &mut Evaluator) -> anyhow::Result<NoneType> {
        let store = eval
            .extra
            .and_then(|extra| extra.downcast_ref::<ProfileStore>())
            .context("Drove evaluator profile store is missing")?;
        store.0.borrow_mut().push(value.to_json_value()?);
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

    let globals = GlobalsBuilder::standard().with(drove_globals).build();
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

    let values = store.0.into_inner();
    if values.is_empty() {
        bail!("Drovefile did not declare any profiles");
    }
    let profiles = values
        .into_iter()
        .map(serde_json::from_value::<Profile>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("invalid profile declaration")?;

    Ok(CompiledDrovefile {
        config: DroveConfig::new(profiles)?,
        repo_root,
        source_digest: canonical_digest(&sources)?,
    })
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
            id = "dev",
            label = "dev",
            tabs = [tab(id = "main", label = "main", layout = pane(id = "shell"))],
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
    fn repository_local_loads_affect_the_source_digest() {
        let directory = tempdir().expect("tempdir");
        fs::write(
            directory.path().join("layout.star"),
            "main_tab = tab(id = \"main\", label = \"main\", layout = pane(id = \"shell\"))",
        )
        .expect("write helper");
        fs::write(
            directory.path().join("Drovefile"),
            r#"
load("layout.star", "main_tab")
profile(
    name = "default",
    workspaces = [workspace(id = "dev", label = "dev", tabs = [main_tab])],
)
"#,
        )
        .expect("write Drovefile");

        let first = compile(&directory.path().join("Drovefile")).expect("compile");
        fs::write(
            directory.path().join("layout.star"),
            "main_tab = tab(id = \"main\", label = \"changed\", layout = pane(id = \"shell\"))",
        )
        .expect("change helper");
        let second = compile(&directory.path().join("Drovefile")).expect("recompile");
        assert_ne!(first.source_digest, second.source_digest);
        assert_eq!(
            second.config.profiles["default"].workspaces[0].tabs[0].label,
            "changed"
        );
    }
}
