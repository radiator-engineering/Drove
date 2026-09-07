//! Backend selection: which backend a project reconciles onto, and which
//! named instance (Herdr session, Radiator hub) it targets (D32, D33).

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use super::{Backend, herdr::HerdrClient, radiator::RadiatorClient};
use crate::model::BackendTargets;

pub const HERDR_BACKEND: &str = "herdr";
pub const RADIATOR_BACKEND: &str = "radiator";
const KNOWN_BACKENDS: &[&str] = &[HERDR_BACKEND, RADIATOR_BACKEND];

/// The resolved instance a backend connects to: an explicit socket override
/// (`--socket`, always wins over everything else) and an optional named
/// target (Herdr session or Radiator hub) resolved per D46's six-level
/// order. `name: None` defers entirely to the backend's own built-in
/// default, including any env fallback it already implements
/// (`HerdrClient`/`RadiatorClient::discover`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Target {
    pub socket: Option<PathBuf>,
    pub name: Option<String>,
}

/// CLI flags relevant to backend selection, gathered by the caller so this
/// module never reads `std::env` or `clap` itself — the D32/D33 precedence
/// test needs no I/O (spec §10).
#[derive(Debug, Clone, Copy, Default)]
pub struct CliInputs<'a> {
    pub backend: Option<&'a str>,
    pub target: Option<&'a str>,
    /// `--session`, a Herdr alias of `--target` (spec §5).
    pub session: Option<&'a str>,
    pub socket: Option<&'a Path>,
}

/// Explicit environment inputs (D46): variables the caller (or a script
/// wrapping `drove`) sets on purpose to say where it wants to go —
/// `DROVE_BACKEND`, and `DROVE_SESSION`/`DROVE_TARGET` mirroring
/// `--session`/`--target`. Ranks above the profile, like the flags they
/// mirror.
#[derive(Debug, Clone, Default)]
pub struct ExplicitEnvInputs {
    pub drove_backend: Option<String>,
    pub drove_session: Option<String>,
    pub drove_target: Option<String>,
}

/// Ambient host environment inputs (D46): variables Herdr/Radiator export
/// into every pane they host, saying where the caller *is*, not where it
/// wants to go — `HERDR_SESSION`, `RADIATOR_HUB`, and the ambient Radiator
/// detection tie-break. Ranks below the file, just above the built-in
/// default.
#[derive(Debug, Clone, Default)]
pub struct AmbientEnvInputs {
    pub herdr_session: Option<String>,
    pub radiator_hub: Option<String>,
    /// Whether an ambient Radiator hub socket is present with no Herdr pane
    /// marker (`radiator::selected_by_environment`), the built-in level's
    /// tie-breaker when nothing else names a backend.
    pub ambient_radiator: bool,
}

/// A profile's own target declaration (D41): `profile(..., session = ...,
/// backend = ...)`. Sits between environment and file in the resolution
/// order for both the backend id and the target name.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProfileInputs<'a> {
    pub backend: Option<&'a str>,
    /// The profile's `session`: a Herdr session name, or (when the resolved
    /// backend is Radiator) the hub name.
    pub session: Option<&'a str>,
}

/// Resolves the backend id and its target instance per D46's six-level
/// order (flag > explicit env > profile > Drovefile > ambient host env >
/// built-in), purely from already-gathered inputs. `--session`/
/// `DROVE_SESSION` (the Herdr alias of `--target`/`DROVE_TARGET`) name a
/// Herdr session, so they are ignored once Radiator is selected —
/// `--target`/`DROVE_TARGET`, `RADIATOR_HUB`, `radiator.hub(...)` and the
/// `main` built-in still apply in that case. Ambient host env variables
/// (`HERDR_SESSION`, `RADIATOR_HUB`) say where the caller is, not where it
/// wants to go, so they rank below the profile and the file, only above the
/// built-in default.
pub fn resolve(
    cli: CliInputs<'_>,
    explicit_env: &ExplicitEnvInputs,
    ambient_env: &AmbientEnvInputs,
    profile: ProfileInputs<'_>,
    file_backend: Option<&str>,
    file_target: &BackendTargets,
) -> (String, Target) {
    let backend = non_empty(cli.backend)
        .or_else(|| non_empty(explicit_env.drove_backend.as_deref()))
        .or_else(|| non_empty(profile.backend))
        .or_else(|| non_empty(file_backend))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if ambient_env.ambient_radiator {
                RADIATOR_BACKEND.to_owned()
            } else {
                HERDR_BACKEND.to_owned()
            }
        });

    let name = match backend.as_str() {
        RADIATOR_BACKEND => non_empty(cli.target)
            .or_else(|| non_empty(explicit_env.drove_target.as_deref()))
            .or_else(|| non_empty(profile.session))
            .or_else(|| non_empty(file_target.radiator_hub.as_deref()))
            .or_else(|| non_empty(ambient_env.radiator_hub.as_deref()))
            .map(str::to_owned),
        _ => non_empty(cli.target.or(cli.session))
            .or_else(|| {
                non_empty(
                    explicit_env
                        .drove_target
                        .as_deref()
                        .or(explicit_env.drove_session.as_deref()),
                )
            })
            .or_else(|| non_empty(profile.session))
            .or_else(|| non_empty(file_target.herdr_session.as_deref()))
            .or_else(|| non_empty(ambient_env.herdr_session.as_deref()))
            .map(str::to_owned),
    };

    (
        backend,
        Target {
            socket: cli.socket.map(Path::to_path_buf),
            name,
        },
    )
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// Opens the selected backend against its resolved target (D33). An unknown
/// backend id fails with the list of known ones.
pub fn open(id: &str, target: &Target) -> Result<Box<dyn Backend>> {
    match id {
        HERDR_BACKEND => Ok(Box::new(HerdrClient::discover(
            target.socket.as_deref(),
            target.name.as_deref(),
        ))),
        RADIATOR_BACKEND => Ok(Box::new(RadiatorClient::discover(
            target.socket.as_deref(),
            target.name.as_deref(),
        ))),
        other => bail!(
            "unknown backend `{other}`; known backends: {}",
            KNOWN_BACKENDS.join(", ")
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn targets(herdr_session: Option<&str>, radiator_hub: Option<&str>) -> BackendTargets {
        BackendTargets {
            herdr_session: herdr_session.map(str::to_owned),
            radiator_hub: radiator_hub.map(str::to_owned),
        }
    }

    #[test]
    fn open_rejects_an_unknown_backend_id() {
        let error = open("nope", &Target::default())
            .err()
            .expect("unknown backend");
        assert!(error.to_string().contains("unknown backend"));
        assert!(error.to_string().contains("herdr"));
        assert!(error.to_string().contains("radiator"));
    }

    #[test]
    fn open_reaches_radiator_when_selected() {
        // The Radiator backend has no Herdr flavor: its `herdr()` accessor
        // (the core/flavor seam, D28) returns `None`.
        let backend = open(RADIATOR_BACKEND, &Target::default()).expect("open radiator");
        assert!(backend.herdr().is_none());
    }

    #[test]
    fn open_reaches_herdr_when_selected() {
        // The Herdr backend offers the Herdr flavor: `herdr()` returns `Some`.
        let backend = open(HERDR_BACKEND, &Target::default()).expect("open herdr");
        assert!(backend.herdr().is_some());
    }

    #[test]
    fn cli_backend_wins_over_every_other_level() {
        let cli = CliInputs {
            backend: Some("radiator"),
            ..Default::default()
        };
        let mut explicit_env = ExplicitEnvInputs {
            drove_backend: Some("herdr".to_owned()),
            ..Default::default()
        };
        let (backend, _) = resolve(
            cli,
            &explicit_env,
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(backend, "radiator");

        explicit_env.drove_backend = None;
        let (backend, _) = resolve(
            cli,
            &explicit_env,
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(backend, "radiator");
    }

    #[test]
    fn explicit_env_backend_wins_over_profile_file_and_built_in() {
        let explicit_env = ExplicitEnvInputs {
            drove_backend: Some("radiator".to_owned()),
            ..Default::default()
        };
        let profile = ProfileInputs {
            backend: Some("herdr"),
            ..Default::default()
        };
        let (backend, _) = resolve(
            CliInputs::default(),
            &explicit_env,
            &AmbientEnvInputs::default(),
            profile,
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(backend, "radiator");
    }

    #[test]
    fn file_backend_wins_over_the_built_in_default() {
        let (backend, _) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            Some("radiator"),
            &BackendTargets::default(),
        );
        assert_eq!(backend, "radiator");
    }

    #[test]
    fn built_in_default_is_herdr_unless_ambient_radiator_is_present() {
        let cli = CliInputs::default();
        let (backend, _) = resolve(
            cli,
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            None,
            &BackendTargets::default(),
        );
        assert_eq!(backend, "herdr");

        let ambient = AmbientEnvInputs {
            ambient_radiator: true,
            ..Default::default()
        };
        let (backend, _) = resolve(
            cli,
            &ExplicitEnvInputs::default(),
            &ambient,
            ProfileInputs::default(),
            None,
            &BackendTargets::default(),
        );
        assert_eq!(backend, "radiator");
    }

    #[test]
    fn herdr_target_name_follows_the_full_precedence_order() {
        // flag > explicit env (DROVE_TARGET/DROVE_SESSION) > profile > file
        // > ambient host env (HERDR_SESSION) > built-in (D46).
        let file = targets(Some("file-session"), None);
        let profile = ProfileInputs {
            session: Some("profile-session"),
            ..Default::default()
        };
        let ambient = AmbientEnvInputs {
            herdr_session: Some("ambient-session".to_owned()),
            ..Default::default()
        };

        let cli = CliInputs {
            target: Some("cli-target"),
            ..Default::default()
        };
        let explicit_env = ExplicitEnvInputs {
            drove_target: Some("explicit-target".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(cli, &explicit_env, &ambient, profile, Some("herdr"), &file);
        assert_eq!(target.name.as_deref(), Some("cli-target"));

        let cli_session_alias = CliInputs {
            session: Some("cli-session"),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli_session_alias,
            &explicit_env,
            &ambient,
            profile,
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("cli-session"));

        let (_, target) = resolve(
            CliInputs::default(),
            &explicit_env,
            &ambient,
            profile,
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("explicit-target"));

        let explicit_session = ExplicitEnvInputs {
            drove_session: Some("explicit-session".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(
            CliInputs::default(),
            &explicit_session,
            &ambient,
            profile,
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("explicit-session"));

        let (_, target) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &ambient,
            profile,
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("profile-session"));

        let (_, target) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &ambient,
            ProfileInputs::default(),
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("file-session"));

        let (_, target) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &ambient,
            ProfileInputs::default(),
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(target.name.as_deref(), Some("ambient-session"));

        let (_, target) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(target.name, None);
    }

    #[test]
    fn radiator_target_name_follows_the_full_precedence_order() {
        let file = targets(None, Some("file-hub"));
        let ambient = AmbientEnvInputs {
            radiator_hub: Some("ambient-hub".to_owned()),
            ..Default::default()
        };
        let cli = CliInputs {
            backend: Some("radiator"),
            target: Some("cli-hub"),
            ..Default::default()
        };
        let explicit_env = ExplicitEnvInputs {
            drove_target: Some("explicit-hub".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli,
            &explicit_env,
            &ambient,
            ProfileInputs::default(),
            None,
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("cli-hub"));

        let cli_no_target = CliInputs {
            backend: Some("radiator"),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli_no_target,
            &explicit_env,
            &ambient,
            ProfileInputs::default(),
            None,
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("explicit-hub"));

        let (_, target) = resolve(
            cli_no_target,
            &ExplicitEnvInputs::default(),
            &ambient,
            ProfileInputs::default(),
            None,
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("file-hub"));

        let (_, target) = resolve(
            cli_no_target,
            &ExplicitEnvInputs::default(),
            &ambient,
            ProfileInputs::default(),
            None,
            &BackendTargets::default(),
        );
        assert_eq!(target.name.as_deref(), Some("ambient-hub"));

        let (_, target) = resolve(
            cli_no_target,
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            None,
            &BackendTargets::default(),
        );
        assert_eq!(target.name, None);
    }

    #[test]
    fn session_flag_and_env_are_ignored_for_the_radiator_backend() {
        // `--session`/`DROVE_SESSION` are the Herdr alias of `--target`/
        // `DROVE_TARGET` (spec §5, D46); they must not leak into the
        // Radiator hub name. `--target`/`DROVE_TARGET`, `RADIATOR_HUB`,
        // `radiator.hub()` and the `main` built-in still apply.
        let file = targets(None, Some("file-hub"));
        let cli = CliInputs {
            backend: Some("radiator"),
            session: Some("some-herdr-session"),
            ..Default::default()
        };
        let explicit_env = ExplicitEnvInputs {
            drove_session: Some("some-herdr-session".to_owned()),
            ..Default::default()
        };
        let (backend, target) = resolve(
            cli,
            &explicit_env,
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            None,
            &file,
        );
        assert_eq!(backend, "radiator");
        assert_eq!(target.name.as_deref(), Some("file-hub"));

        let ambient = AmbientEnvInputs {
            radiator_hub: Some("ambient-hub".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli,
            &explicit_env,
            &ambient,
            ProfileInputs::default(),
            None,
            &BackendTargets::default(),
        );
        assert_eq!(target.name.as_deref(), Some("ambient-hub"));

        let cli_with_target = CliInputs {
            backend: Some("radiator"),
            session: Some("some-herdr-session"),
            target: Some("cli-hub"),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli_with_target,
            &explicit_env,
            &ambient,
            ProfileInputs::default(),
            None,
            &BackendTargets::default(),
        );
        assert_eq!(target.name.as_deref(), Some("cli-hub"));
    }

    #[test]
    fn profile_backend_wins_over_file_but_loses_to_explicit_env_and_cli() {
        let profile = ProfileInputs {
            backend: Some("radiator"),
            ..Default::default()
        };

        let (backend, _) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            profile,
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(backend, "radiator");

        let explicit_env = ExplicitEnvInputs {
            drove_backend: Some("herdr".to_owned()),
            ..Default::default()
        };
        let (backend, _) = resolve(
            CliInputs::default(),
            &explicit_env,
            &AmbientEnvInputs::default(),
            profile,
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(backend, "herdr");

        let cli = CliInputs {
            backend: Some("herdr"),
            ..Default::default()
        };
        let (backend, _) = resolve(
            cli,
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            profile,
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(backend, "herdr");
    }

    #[test]
    fn profile_session_wins_over_file_but_loses_to_explicit_env_and_cli_for_herdr() {
        let file = targets(Some("file-session"), None);
        let profile = ProfileInputs {
            session: Some("profile-session"),
            ..Default::default()
        };

        let (_, target) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            profile,
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("profile-session"));

        let explicit_env = ExplicitEnvInputs {
            drove_session: Some("explicit-session".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(
            CliInputs::default(),
            &explicit_env,
            &AmbientEnvInputs::default(),
            profile,
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("explicit-session"));

        let cli = CliInputs {
            target: Some("cli-target"),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli,
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            profile,
            Some("herdr"),
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("cli-target"));
    }

    #[test]
    fn profile_session_wins_over_file_but_loses_to_explicit_env_and_cli_for_radiator() {
        let file = targets(None, Some("file-hub"));
        let profile = ProfileInputs {
            session: Some("profile-hub"),
            ..Default::default()
        };
        let cli_radiator = CliInputs {
            backend: Some("radiator"),
            ..Default::default()
        };

        let (_, target) = resolve(
            cli_radiator,
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            profile,
            None,
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("profile-hub"));

        let explicit_env = ExplicitEnvInputs {
            drove_target: Some("explicit-hub".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli_radiator,
            &explicit_env,
            &AmbientEnvInputs::default(),
            profile,
            None,
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("explicit-hub"));

        let cli_with_target = CliInputs {
            backend: Some("radiator"),
            target: Some("cli-hub"),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli_with_target,
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            profile,
            None,
            &file,
        );
        assert_eq!(target.name.as_deref(), Some("cli-hub"));
    }

    #[test]
    fn profile_session_wins_over_ambient_herdr_session_d46() {
        // The regression D46 fixes: `drove monitoring` run from inside
        // Herdr session `drove` must target the profile's own session, not
        // the ambient one it happens to be running in.
        let profile = ProfileInputs {
            session: Some("drove-mon"),
            ..Default::default()
        };
        let ambient = AmbientEnvInputs {
            herdr_session: Some("drove".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(
            CliInputs::default(),
            &ExplicitEnvInputs::default(),
            &ambient,
            profile,
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(target.name.as_deref(), Some("drove-mon"));

        let explicit_env = ExplicitEnvInputs {
            drove_session: Some("x".to_owned()),
            ..Default::default()
        };
        let (_, target) = resolve(
            CliInputs::default(),
            &explicit_env,
            &ambient,
            profile,
            Some("herdr"),
            &BackendTargets::default(),
        );
        assert_eq!(target.name.as_deref(), Some("x"));
    }

    #[test]
    fn explicit_socket_is_carried_through_regardless_of_target_name() {
        let cli = CliInputs {
            socket: Some(Path::new("/tmp/explicit.sock")),
            ..Default::default()
        };
        let (_, target) = resolve(
            cli,
            &ExplicitEnvInputs::default(),
            &AmbientEnvInputs::default(),
            ProfileInputs::default(),
            None,
            &BackendTargets::default(),
        );
        assert_eq!(target.socket, Some(PathBuf::from("/tmp/explicit.sock")));
    }
}
