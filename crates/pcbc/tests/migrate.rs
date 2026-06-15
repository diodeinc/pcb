use pcb_test_utils::sandbox::Sandbox;
use pcb_zen_core::config::pcb_version_from_cargo;
use std::fs;

#[test]
fn migrate_bumps_workspace_pcb_version_to_current_lane() {
    let target = pcb_version_from_cargo();
    let previous = if target == "0.3" { "0.2" } else { "0.3" };
    let mut sandbox = Sandbox::new();
    sandbox.write(
        "pcb.toml",
        format!(
            r#"# workspace comment
[workspace]
name = "demo"
pcb-version = "{previous}" # old lane

[dependencies]
"#
        ),
    );

    run_migrate(&mut sandbox);

    let content = fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap();
    assert!(content.contains("# workspace comment"));
    assert!(content.contains(&format!("pcb-version = \"{target}\" # old lane")));
}

#[test]
fn migrate_leaves_current_workspace_manifest_unchanged() {
    let target = pcb_version_from_cargo();
    let original = format!("[workspace]\npcb-version = \"{target}\"\nname = \"demo\"\n");
    let mut sandbox = Sandbox::new();
    sandbox.write("pcb.toml", &original);

    run_migrate(&mut sandbox);

    let content = fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap();
    assert_eq!(content, original);
}

#[test]
fn migrate_removes_deprecated_workspace_members() {
    let target = pcb_version_from_cargo();
    let original = format!(
        r#"[workspace]
pcb-version = "{target}"
members = ["boards/*"]
name = "demo"
"#
    );
    let mut sandbox = Sandbox::new();
    sandbox.write("pcb.toml", original);

    run_migrate(&mut sandbox);

    let content = fs::read_to_string(sandbox.root_path().join("pcb.toml")).unwrap();
    assert!(content.contains(&format!("pcb-version = \"{target}\"")));
    assert!(content.contains("name = \"demo\""));
    assert!(!content.contains("members"));
}

fn run_migrate(sandbox: &mut Sandbox) {
    let output = sandbox
        .run("pcbc", ["migrate"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .unwrap();
    assert!(
        output.status.success(),
        "pcbc migrate failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
