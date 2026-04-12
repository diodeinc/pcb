#![cfg(not(target_os = "windows"))]

use pcb_test_utils::sandbox::Sandbox;
use std::process::Output;

const SIMPLE_RESISTOR_V1: &str = r#"
value = config("value", str, default = "1kOhm")

P1 = io("P1", Net)
P2 = io("P2", Net)
"#;

const SIMPLE_RESISTOR_V2: &str = r#"
value = config("value", str, default = "4.7kOhm")

P1 = io("P1", Net)
P2 = io("P2", Net)
"#;

const ALLOWED_CONFIG_MODULE: &str = r#"
package = config(
    "package",
    str,
    allowed = ["0402", "0603"],
    default = "0603",
)
"#;

fn seed_remote_package(sb: &mut Sandbox) {
    let mut fixture = sb.git_fixture("https://github.com/mycompany/components.git");
    fixture
        .write("SimpleResistor/pcb.toml", "")
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_V1)
        .commit("Add SimpleResistor v1")
        .tag("SimpleResistor/v1.0.0", false)
        .write("SimpleResistor/SimpleResistor.zen", SIMPLE_RESISTOR_V2)
        .commit("Add SimpleResistor v2")
        .tag("SimpleResistor/v2.0.0", false)
        .push_mirror();
}

fn run_doc(sb: &mut Sandbox, package: &str) -> Output {
    sb.run("pcb", ["doc", "--package", package])
        .stderr_capture()
        .stdout_capture()
        .unchecked()
        .run()
        .expect("doc command failed")
}

#[test]
fn test_pcb_doc_remote_package_defaults_to_latest() {
    let mut sb = Sandbox::new();
    seed_remote_package(&mut sb);

    let default_output = run_doc(&mut sb, "github.com/mycompany/components/SimpleResistor");
    let latest_output = run_doc(
        &mut sb,
        "github.com/mycompany/components/SimpleResistor@latest",
    );
    let pinned_output = run_doc(
        &mut sb,
        "github.com/mycompany/components/SimpleResistor@1.0.0",
    );

    for (label, output) in [
        ("default", &default_output),
        ("latest", &latest_output),
        ("pinned", &pinned_output),
    ] {
        assert!(
            output.status.success(),
            "{label} command failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).trim().is_empty(),
            "{label} command wrote to stderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let default_stdout = String::from_utf8_lossy(&default_output.stdout);
    let latest_stdout = String::from_utf8_lossy(&latest_output.stdout);
    let pinned_stdout = String::from_utf8_lossy(&pinned_output.stdout);

    assert_eq!(
        default_stdout, latest_stdout,
        "default remote doc output should match @latest"
    );
    assert!(
        default_stdout.contains("| value | str | \"4.7kOhm\" |"),
        "default output should document the latest tag:\n{default_stdout}"
    );
    assert!(
        !default_stdout.contains("| value | str | \"1kOhm\" |"),
        "default output should not document the older tag:\n{default_stdout}"
    );
    assert!(
        pinned_stdout.contains("| value | str | \"1kOhm\" |"),
        "explicit version should still resolve the older tag:\n{pinned_stdout}"
    );
    assert!(
        !pinned_stdout.contains("| value | str | \"4.7kOhm\" |"),
        "explicit version should not resolve the newer tag:\n{pinned_stdout}"
    );
}

#[test]
fn test_pcb_doc_shows_allowed_values_for_config() {
    let mut sb = Sandbox::new();
    sb.write("pcb.toml", "")
        .write("Widget.zen", ALLOWED_CONFIG_MODULE);

    let output = run_doc(&mut sb, ".");

    assert!(
        output.status.success(),
        "doc command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).trim().is_empty(),
        "doc command wrote to stderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = sb.sanitize_output(&String::from_utf8_lossy(&output.stdout));
    insta::assert_snapshot!("doc_allowed_values", stdout);
}
