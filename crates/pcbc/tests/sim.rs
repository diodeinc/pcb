#![cfg(unix)]

use pcb_test_utils::sandbox::Sandbox;
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn sim_runs_ngspice_in_source_directory() {
    let mut sandbox = Sandbox::new().with_workspace();
    let source = r#"
builtin.set_sim_setup(content="V1 vin 0 DC 5\nR1 vin 0 1000\n.op\n.end\n")
"#;
    sandbox
        .write("bench.zen", source)
        .write("nested/bench.zen", source)
        .write(
            "ngspice",
            r#"#!/bin/sh
set -eu
if [ "$1" = "--version" ]; then
    exit 0
fi
test "$#" -eq 2
test "$1" = "-b"
grep -q '^R1 vin 0 1000$' "$2"
printf 'ngspice workdir: '
pwd -P
"#,
        );
    let ngspice = sandbox.root_path().join("ngspice");
    fs::set_permissions(&ngspice, fs::Permissions::from_mode(0o755)).unwrap();
    sandbox.env("NGSPICE", ngspice.to_str().unwrap());

    let root = sandbox.root_path().canonicalize().unwrap();
    let absolute_source = root.join("bench.zen");
    for (path, expected_dir) in [
        ("bench.zen", root.clone()),
        ("./bench.zen", root.clone()),
        (absolute_source.to_str().unwrap(), root.clone()),
        ("nested/bench.zen", root.join("nested")),
    ] {
        let output = sandbox
            .run("pcbc", ["sim", "--offline", "--verbose", path])
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .expect("run pcb sim");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "pcb sim {path} failed: {stderr}");
        assert!(
            stderr.contains(&format!("ngspice workdir: {}\n", expected_dir.display())),
            "pcb sim {path} used the wrong working directory: {stderr}"
        );
    }
}
