//! D27/D28: the core modules must not name the Herdr-only concept `tab`.
//!
//! The `Backend` trait is the intersection of what every backend does (D27);
//! tabs are a Herdr flavor, reached through `HerdrExt`/`HerdrAction`/
//! `Placement::Herdr` (D28, D29). So the four core modules — `backend/mod.rs`,
//! `planner.rs`, `ir.rs`, `executor.rs` — may say `tab` only where they name
//! that Herdr flavor. This test reads each file and fails if the bare word
//! `tab` appears on any line that is not doing so.
//!
//! The rule is whole-word and case-sensitive: `Tab`, `create_tab`, `tab_id`
//! and `.tabs` are not the bare identifier and never trip it. A line is allowed
//! when it names the Herdr flavor (`Placement`, `Herdr`) or is placement-field
//! syntax (`tab:`), which is exactly where the seam permits the word.

use std::path::PathBuf;

const CORE_MODULES: &[&str] = &[
    "src/backend/mod.rs",
    "src/planner.rs",
    "src/ir.rs",
    "src/executor.rs",
];

/// True if `line` contains the whole word `tab` (case-sensitive): `tab`
/// bounded on each side by something other than a letter, digit or `_`, so
/// `Tab`, `tabs`, `create_tab` and `tab_id` do not match.
fn has_bare_tab(line: &str) -> bool {
    let bytes = line.as_bytes();
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut i = 0;
    while let Some(found) = line[i..].find("tab") {
        let start = i + found;
        let end = start + 3;
        let before_ok = start == 0 || !is_word(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        i = start + 1;
    }
    false
}

/// A bare-`tab` line is allowed only where it names the Herdr flavor the seam
/// routes tabs through, or is the `tab:` placement field.
fn line_is_allowed(line: &str) -> bool {
    line.contains("Placement") || line.contains("Herdr") || line.contains("tab:")
}

#[test]
fn core_modules_do_not_name_the_herdr_tab_concept() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();

    for module in CORE_MODULES {
        let path = root.join(module);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {module}: {error}"));
        for (number, line) in text.lines().enumerate() {
            if has_bare_tab(line) && !line_is_allowed(line) {
                offenders.push(format!("{module}:{}: {}", number + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the core modules must not use the bare identifier `tab` (D27/D28); \
         route it through the Herdr flavor instead:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_grep_rule_catches_a_bare_tab_but_not_its_look_alikes() {
    // Guards the matcher itself, so a future edit cannot quietly weaken it.
    assert!(has_bare_tab("let tab = 1;"));
    assert!(has_bare_tab("close the tab"));
    assert!(!has_bare_tab("let Tab = 1;"));
    assert!(!has_bare_tab("create_tab()"));
    assert!(!has_bare_tab("self.tabs"));
    assert!(!has_bare_tab("tab_id"));

    assert!(line_is_allowed("Placement::Herdr { tab, split, ratios }"));
    assert!(line_is_allowed("    tab: String,"));
    assert!(!line_is_allowed("let tab = first();"));
}
