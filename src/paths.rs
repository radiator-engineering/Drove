//! Cross-platform path rendering for content digests (D21, D36).
//!
//! A declared `cwd` goes into a resource's digest content as a plain
//! string. `PathBuf`'s own `Display` uses the host's native separator, so
//! the same declared path digests differently depending on which OS ran
//! `drove`. [`normalize_for_digest`] renders a path the same way on every
//! OS: it treats both `/` and `\` as separators (Windows-authored paths
//! checked out on Unix would otherwise parse as one opaque component,
//! since Unix does not treat `\` as a separator), then re-joins the
//! segments with a single explicit `/`.
//!
//! This renders segments directly from the separator-normalized string
//! rather than through `Path::components()`: `Component::RootDir`'s own
//! `OsStr` is the separator character itself, so mapping every component
//! through `as_os_str()` and joining with `/` doubles it for an absolute
//! path (`/repo/sub` became `//repo/sub`, `C:\repo\sub` became
//! `C://repo/sub`) — host-dependent digest content, exactly what this
//! module exists to prevent.
//!
//! The root prefix is rendered explicitly, before segments are joined, so
//! different kinds of root can't collide: a POSIX absolute path (`/repo`),
//! a Windows drive-absolute path (`C:/repo`) and a UNC path (`//server/repo`,
//! from `\\server\repo`) each keep a distinct prefix. Naively stripping `\\`
//! down to a single `/` would otherwise render a UNC path identically to a
//! POSIX absolute path with the same segments.

use std::path::Path;

pub fn normalize_for_digest(path: &Path) -> String {
    let forward = path.to_string_lossy().replace('\\', "/");
    let (prefix, rest) = root_prefix(&forward);
    let joined = rest
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    format!("{prefix}{joined}")
}

/// Splits a separator-normalized path into its root prefix, rendered so a
/// different kind of root can never collide with it, and the remaining
/// segments still to be joined.
fn root_prefix(forward: &str) -> (String, &str) {
    if let Some(rest) = forward.strip_prefix("//") {
        return ("//".to_owned(), rest);
    }
    if let Some(rest) = forward.strip_prefix('/') {
        return ("/".to_owned(), rest);
    }
    let bytes = forward.as_bytes();
    let has_drive_letter =
        bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':');
    if has_drive_letter {
        let drive = &forward[..2];
        let rest = forward[2..].strip_prefix('/').unwrap_or(&forward[2..]);
        return (format!("{drive}/"), rest);
    }
    (String::new(), forward)
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

    /// Exact-output regression for the double-separator bug: an absolute
    /// path must render with exactly one leading `/`, not two, and the
    /// literal expected string is the same on every OS this runs on.
    #[test]
    fn absolute_paths_render_with_a_single_leading_slash() {
        assert_eq!(normalize_for_digest(Path::new("/repo/sub")), "/repo/sub");
        assert_eq!(
            normalize_for_digest(Path::new("C:\\repo\\sub")),
            "C:/repo/sub"
        );
    }

    /// Exact-output regression: a Windows drive-root, a bare drive letter,
    /// and a UNC path each render to a distinct, stable value, and none of
    /// them collides with a POSIX absolute path of the same segments.
    #[test]
    fn windows_root_and_unc_paths_do_not_collide_with_posix_absolute_paths() {
        assert_eq!(normalize_for_digest(Path::new("C:\\")), "C:/");
        assert_eq!(normalize_for_digest(Path::new("C:")), "C:/");
        assert_eq!(
            normalize_for_digest(Path::new("\\\\server\\share")),
            "//server/share"
        );
        assert_ne!(
            normalize_for_digest(Path::new("\\\\server\\share")),
            normalize_for_digest(Path::new("/server/share"))
        );
    }

    /// D30: content digests must stay stable across this change.
    /// `examples/basic` declares no explicit `cwd`, so every pane and
    /// workspace falls back to the default (`.` / `None`), which
    /// `normalize_for_digest` renders the same way as the unnormalized
    /// `PathBuf` did. Digests recorded from `src/ir.rs` with the two `cwd`
    /// call sites reverted to the raw `PathBuf` field, on this same base.
    ///
    /// D53 changed a resource's `digest` from one hash of its whole content
    /// to a composite JSON object of per-category hashes (`label`, `cwd`,
    /// `env`, ...), so the planner can tell which category changed since the
    /// last apply instead of only that something did. The category hashes
    /// below are pinned the same way the old single hash was; each one is
    /// still exactly the pre-D53 `content_digest` of its own category, so
    /// this test still catches `normalize_for_digest` drift.
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
                    "{\"children\":\"d14289f9153a213c9478c2ae0e0c1bf605bb0a5f9ba5042b20878ebbb3edc8cd\",\"cwd\":\"c6957470f233389737b86d8e27886a0f7e39b5da5a35a98e58ee28675e064a52\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\"}"
                ),
                (
                    "editor",
                    "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"728c981e3aad62fd5b893e8a2ab14c0d7dde019d6dcef6d0d71248d5a3225ceb\"}"
                ),
                (
                    "tests",
                    "{\"cwd\":\"59911c573c6d7f60fac16b5a657027e3355ba9758f0e74f12bff2ea7e5601b99\",\"env\":\"460e3a1e3343b944d6c45008cf5c70047a11ce4396ff1f399d426ba392fcf442\",\"label\":\"f86921faad5a509c7866edc54ace4be6e5e54d9f36d0af208204a81dc4ddf962\",\"on_start\":\"0a3aba4b6279d8dd2f5186753b99d6f38925ba859d64cf683f7f435a312cbbc0\",\"other\":\"2d008dd52867a5dd6c3c6cf09c7fa56b722e448094db894417f2cd3a5ce689c2\",\"serve\":\"728c981e3aad62fd5b893e8a2ab14c0d7dde019d6dcef6d0d71248d5a3225ceb\"}"
                ),
            ]
        );
    }
}
