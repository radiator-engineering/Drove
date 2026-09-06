//! Cross-platform path rendering for content digests (D21, D36).
//!
//! A declared `cwd` goes into a resource's digest content as a plain
//! string. `PathBuf`'s own `Display` uses the host's native separator, so
//! the same declared path digests differently depending on which OS ran
//! `drove`. [`normalize_for_digest`] renders a path the same way on every
//! OS: it treats both `/` and `\` as separators (Windows-authored paths
//! checked out on Unix would otherwise parse as one opaque component,
//! since Unix does not treat `\` as a separator), then re-renders each
//! component joined with `/`.

use std::path::Path;

pub fn normalize_for_digest(path: &Path) -> String {
    let forward = path.to_string_lossy().replace('\\', "/");
    Path::new(&forward)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_and_unix_paths_with_the_same_components_render_identically() {
        let windows = normalize_for_digest(Path::new("C:\\repo\\sub"));
        let unix = normalize_for_digest(Path::new("C:/repo/sub"));
        assert_eq!(windows, unix);
    }

    #[test]
    fn relative_windows_style_path_matches_relative_unix_style_path() {
        let windows = normalize_for_digest(Path::new("sub\\dir\\leaf"));
        let unix = normalize_for_digest(Path::new("sub/dir/leaf"));
        assert_eq!(windows, unix);
    }

    #[test]
    fn current_dir_renders_unchanged() {
        assert_eq!(normalize_for_digest(Path::new(".")), ".");
    }

    /// D30: content digests must stay stable across this change.
    /// `examples/basic` declares no explicit `cwd`, so every pane and
    /// workspace falls back to the default (`.` / `None`), which
    /// `normalize_for_digest` renders the same way as the unnormalized
    /// `PathBuf` did. Digests recorded from `src/ir.rs` with the two `cwd`
    /// call sites reverted to the raw `PathBuf` field, on this same base.
    #[test]
    fn examples_basic_digest_is_unchanged_by_cwd_normalization() {
        let compiled = crate::dsl::compile(Path::new("examples/basic/Drovefile"))
            .expect("examples/basic compiles");
        let profile = compiled
            .config
            .profiles
            .get("default")
            .expect("examples/basic declares a `default` profile");
        let ir = profile.to_ir();
        let digests: Vec<(&str, &str)> = ir
            .resources
            .iter()
            .map(|resource| (resource.name.as_str(), resource.digest.as_str()))
            .collect();
        assert_eq!(
            digests,
            vec![
                (
                    "development",
                    "195d182808ab3bcbfe0f098ab4159b2dff2635a8433070c7e03f0967155ce01a"
                ),
                (
                    "editor",
                    "999810892ca9ffcf59d6e38630d7291fc4a7968d6e844da5f2f3f2b1a33e423a"
                ),
                (
                    "tests",
                    "999810892ca9ffcf59d6e38630d7291fc4a7968d6e844da5f2f3f2b1a33e423a"
                ),
            ]
        );
    }
}
