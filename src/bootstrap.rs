//! Idempotent, explicitly approved bootstrap tasks.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::{Command, ExitStatus},
};

use anyhow::{Context, Result, bail};

use crate::{
    model::{BootstrapTask, Profile},
    planner::{Action, bootstrap_action},
    state::LocalState,
};

pub fn pending_actions(
    profile: &Profile,
    repo_root: &Path,
    source_digest: &str,
    state: &LocalState,
) -> Result<Vec<Action>> {
    let mut actions = Vec::new();
    for task in ordered_tasks(&profile.bootstrap)? {
        let digest = task_digest(task, repo_root, source_digest)?;
        if !state.is_approved(&digest) {
            actions.push(bootstrap_action(
                task,
                "bootstrap task bytes require approval before check or run",
            )?);
            continue;
        }
        match run_argv(&task.check, repo_root) {
            Ok(status) if status.success() => {}
            Ok(status) => actions.push(bootstrap_action(
                task,
                &format!("bootstrap check exited with {status}"),
            )?),
            Err(error) => actions.push(bootstrap_action(
                task,
                &format!("bootstrap check could not run: {error}"),
            )?),
        }
    }
    Ok(actions)
}

pub fn task_digest(task: &BootstrapTask, repo_root: &Path, source_digest: &str) -> Result<String> {
    task.digest(repo_root, source_digest)
}

pub fn execute_task(
    task: &BootstrapTask,
    repo_root: &Path,
    source_digest: &str,
    state: &mut LocalState,
    approved_now: bool,
) -> Result<()> {
    let digest = task_digest(task, repo_root, source_digest)?;
    if !state.is_approved(&digest) {
        if !approved_now {
            bail!("bootstrap task `{}` changed and requires approval", task.id);
        }
        state.approve(digest);
        state.save()?;
    }

    if run_argv(&task.check, repo_root)
        .with_context(|| format!("cannot check bootstrap task `{}`", task.id))?
        .success()
    {
        return Ok(());
    }

    let status = run_argv(&task.run, repo_root)
        .with_context(|| format!("cannot run bootstrap task `{}`", task.id))?;
    if !status.success() {
        bail!("bootstrap task `{}` exited with {status}", task.id);
    }
    let check = run_argv(&task.check, repo_root)
        .with_context(|| format!("cannot verify bootstrap task `{}`", task.id))?;
    if !check.success() {
        bail!(
            "bootstrap task `{}` did not satisfy its check after running",
            task.id
        );
    }
    Ok(())
}

fn run_argv(argv: &[String], cwd: &Path) -> Result<ExitStatus> {
    let (program, args) = argv
        .split_first()
        .context("command argv must not be empty")?;
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("DROVE_BOOTSTRAP", "1")
        .status()
        .with_context(|| format!("failed to execute `{program}`"))
}

fn ordered_tasks(tasks: &[BootstrapTask]) -> Result<Vec<&BootstrapTask>> {
    let by_id = tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect::<BTreeMap<_, _>>();
    let mut visited = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut ordered = Vec::new();

    fn visit<'a>(
        id: &'a str,
        by_id: &BTreeMap<&'a str, &'a BootstrapTask>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
        ordered: &mut Vec<&'a BootstrapTask>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            bail!("bootstrap dependency cycle includes `{id}`");
        }
        let task = by_id
            .get(id)
            .with_context(|| format!("unknown bootstrap task `{id}`"))?;
        for dependency in &task.depends_on {
            visit(dependency, by_id, visiting, visited, ordered)?;
        }
        visiting.remove(id);
        visited.insert(id);
        ordered.push(*task);
        Ok(())
    }

    for id in by_id.keys().copied() {
        visit(id, &by_id, &mut visiting, &mut visited, &mut ordered)?;
    }
    Ok(ordered)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn orders_bootstrap_dependencies() {
        let tasks = vec![
            BootstrapTask {
                id: "second".into(),
                check: vec!["true".into()],
                run: vec!["true".into()],
                inputs: vec![],
                depends_on: vec!["first".into()],
            },
            BootstrapTask {
                id: "first".into(),
                check: vec!["true".into()],
                run: vec!["true".into()],
                inputs: vec![],
                depends_on: vec![],
            },
        ];
        let ordered = ordered_tasks(&tasks).expect("ordered");
        assert_eq!(ordered[0].id, "first");
        assert_eq!(ordered[1].id, "second");
    }

    #[test]
    fn unapproved_task_does_not_execute_its_check() {
        let directory = tempfile::tempdir().expect("tempdir");
        let profile = Profile {
            name: "default".into(),
            workspaces: vec![],
            agents: vec![],
            bootstrap: vec![BootstrapTask {
                id: "guarded".into(),
                check: vec!["definitely-not-a-real-executable".into()],
                run: vec!["also-not-real".into()],
                inputs: vec![],
                depends_on: vec![],
            }],
        };
        let state = LocalState {
            schema_version: 1,
            repo_root: directory.path().to_owned(),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: vec![],
            path: PathBuf::new(),
        };
        let actions =
            pending_actions(&profile, directory.path(), "source", &state).expect("actions");
        assert_eq!(actions.len(), 1);
        assert!(actions[0].reason.contains("require approval"));
    }

    #[test]
    fn approval_happens_before_check_and_skips_satisfied_run() {
        let directory = tempfile::tempdir().expect("tempdir");
        let task = BootstrapTask {
            id: "already-ready".into(),
            check: vec!["cargo".into(), "--version".into()],
            run: vec!["definitely-not-a-real-executable".into()],
            inputs: vec![],
            depends_on: vec![],
        };
        let mut state = LocalState {
            schema_version: 1,
            repo_root: directory.path().to_owned(),
            profiles: BTreeMap::new(),
            approvals: BTreeSet::new(),
            journal: vec![],
            path: directory.path().join("state.json"),
        };
        execute_task(&task, directory.path(), "source", &mut state, true).expect("satisfied task");
        let digest = task_digest(&task, directory.path(), "source").expect("digest");
        assert!(state.is_approved(&digest));
    }
}
