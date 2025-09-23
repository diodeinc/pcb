mod common;
use common::TestProject;

#[test]
fn snapshot_module_loader_attrs() {
    let env = TestProject::new();

    env.add_files_from_blob(
        r#"
# --- sub.zen
# Declare optional placeholders without explicit defaults
TestExport = "test"

# --- top.zen
# Import `sub` module with alias `Sub`.
Sub = Module("sub.zen")

check(Sub.TestExport == "test", "TestExport should be 'test'")
"#,
    );

    star_snapshot!(env, "top.zen");
}
