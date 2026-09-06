use std::process::Command;

use serde_json::Value;

#[test]
fn installed_herdr_schema_contains_drove_methods() {
    let output = match Command::new("herdr")
        .args(["api", "schema", "--json"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };
    let schema: Value = serde_json::from_slice(&output.stdout).expect("Herdr schema JSON");
    let text = serde_json::to_string(&schema).expect("schema text");
    for method in [
        "session.snapshot",
        "workspace.create",
        "workspace.rename",
        "tab.rename",
        "layout.export",
        "layout.apply",
        "agent.start",
        "workspace.report_metadata",
    ] {
        assert!(text.contains(method), "Herdr schema omitted {method}");
    }
}
