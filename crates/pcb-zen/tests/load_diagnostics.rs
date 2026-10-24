use crate::common;
use common::TestProject;

#[test]
#[cfg(not(target_os = "windows"))]
fn snapshot_load_nonexistent_file() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
# This load should fail and the error should point to this line
load("./nonexistent.zen", "foo")

print("This shouldn't execute")
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn snapshot_nested_load_errors() {
    let env = TestProject::new();

    env.add_file(
        "level3.zen",
        r#"
# This file has an actual error
undefined_variable + 1
"#,
    );

    env.add_file(
        "level2.zen",
        r#"
# This loads a file with an error
load("./level3.zen", "something")
"#,
    );

    env.add_file(
        "level1.zen",
        r#"
# This loads a file that loads a file with an error
load("./level2.zen", "something")
"#,
    );

    env.add_file(
        "test.zen",
        r#"
# Top level load - error should propagate up with proper spans
load("./level1.zen", "something")
"#,
    );

    star_snapshot!(env, "test.zen");
}

#[test]
fn snapshot_cyclic_load_error() {
    let env = TestProject::new();

    env.add_file(
        "a.zen",
        r#"
# This creates a cycle: a -> b -> a
load("./b.zen", "b_func")

def a_func():
    return "a"
"#,
    );

    env.add_file(
        "b.zen",
        r#"
# This completes the cycle
load("./a.zen", "a_func")

def b_func():
    return "b"
"#,
    );

    star_snapshot!(env, "a.zen");
}

#[test]
#[cfg(unix)]
fn load_cannot_read_files_outside_workspace() {
    let env = TestProject::new();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().canonicalize().unwrap().join("secret.zen");
    std::fs::write(&secret, "API_KEY hunter2\n").unwrap();
    std::os::unix::fs::symlink(&secret, env.root().join("link.zen")).unwrap();
    env.add_file("lib/ok.zen", "OK = 1\n");
    let outside_dir = secret
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();

    let ok = env.root().join("lib/ok.zen");
    let rejected = [
        secret.display().to_string(),
        format!("@stdlib/{}", secret.display()),
        format!("package://workspace/../{outside_dir}/secret.zen"),
        env.root().join("link.zen").display().to_string(),
        "package://workspace/link.zen".to_string(),
    ];
    for spec in &rejected {
        env.add_file("top.zen", &format!("load(\"{spec}\", \"x\")\n"));
        let rendered =
            pcb_zen::diagnostics::render_diagnostics_to_string(&env.eval("top.zen").diagnostics);
        assert!(
            rendered.contains("resolves outside the workspace") && !rendered.contains("hunter2"),
            "{spec}: {rendered}"
        );
    }

    // Absolute paths inside the workspace still load.
    env.add_file("top.zen", &format!("load(\"{}\", \"OK\")\n", ok.display()));
    let result = env.eval("top.zen");
    assert!(
        !result.diagnostics.iter().any(|d| d.is_error()),
        "{:?}",
        result.diagnostics
    );
}
