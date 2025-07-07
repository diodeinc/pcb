use pcb_zen::create_eval_context;
use pcb_zen_core::InputMap;
use tempfile::TempDir;

#[test]
fn test_module_with_local_alias() {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    // Create a local "stdlib" directory structure
    let stdlib_dir = workspace_root.join("packages").join("stdlib");
    std::fs::create_dir_all(&stdlib_dir).unwrap();

    // Create a simple module in the local stdlib
    let resistor_file = stdlib_dir.join("Resistor.zen");
    std::fs::write(
        &resistor_file,
        r#"
# A simple resistor module
P1 = io("P1", Net)
P2 = io("P2", Net)
value = config("value", str, default="1kohm")
"#,
    )
    .unwrap();

    // Create a pcb.toml with local alias
    let pcb_toml = workspace_root.join("pcb.toml");
    std::fs::write(
        &pcb_toml,
        r#"
[packages]
stdlib = "./packages/stdlib"
"#,
    )
    .unwrap();

    // Create a test file that uses Module() with the local alias
    let test_file = workspace_root.join("test.zen");
    std::fs::write(
        &test_file,
        r#"
# Test that Module() works with local package alias
Resistor = Module("@stdlib/Resistor.zen")

Resistor(
    name = "R1",
    P1 = Net("VCC"),
    P2 = Net("GND"),
    value = "10kohm",
)
"#,
    )
    .unwrap();

    // Create an evaluation context with proper load resolver setup
    let ctx = create_eval_context(workspace_root);

    // Evaluate the test file
    let result = ctx
        .set_source_path(test_file.clone())
        .set_module_name("test".to_string())
        .set_inputs(InputMap::new())
        .eval();

    // Check that the evaluation succeeded
    assert!(result.output.is_some(), "Evaluation should succeed");
    assert!(result.diagnostics.is_empty(), "Should have no diagnostics");

    // Verify the module was loaded and instantiated
    if let Some(output) = result.output {
        let module_children = output.sch_module.children();
        assert_eq!(module_children.len(), 1, "Should have one child module");
    }
}

#[test]
fn test_module_with_local_alias_relative_path() {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    // Create a subdirectory structure
    let sub_dir = workspace_root.join("src");
    std::fs::create_dir_all(&sub_dir).unwrap();

    // Create a local modules directory
    let modules_dir = workspace_root.join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();

    // Create a simple module
    let module_file = modules_dir.join("MyModule.zen");
    std::fs::write(
        &module_file,
        r#"
# A simple module
input = io("input", Net)
output = io("output", Net)
"#,
    )
    .unwrap();

    // Create a pcb.toml with relative path alias
    let pcb_toml = workspace_root.join("pcb.toml");
    std::fs::write(
        &pcb_toml,
        r#"
[packages]
local = "./modules"
"#,
    )
    .unwrap();

    // Create a test file in the subdirectory that uses the alias
    let test_file = sub_dir.join("test.zen");
    std::fs::write(
        &test_file,
        r#"
# Test that Module() works with relative path alias
MyModule = Module("@local/MyModule.zen")

MyModule(
    name = "M1",
    input = Net("IN"),
    output = Net("OUT"),
)
"#,
    )
    .unwrap();

    // Create an evaluation context with proper load resolver setup
    let ctx = create_eval_context(workspace_root);

    // Evaluate the test file
    let result = ctx
        .set_source_path(test_file.clone())
        .set_module_name("test".to_string())
        .set_inputs(InputMap::new())
        .eval();

    // Check that the evaluation succeeded
    assert!(result.output.is_some(), "Evaluation should succeed");
    assert!(result.diagnostics.is_empty(), "Should have no diagnostics");
}

#[test]
fn test_module_with_nonexistent_local_alias() {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    // Create a pcb.toml with alias pointing to nonexistent directory
    let pcb_toml = workspace_root.join("pcb.toml");
    std::fs::write(
        &pcb_toml,
        r#"
[packages]
missing = "./does_not_exist"
"#,
    )
    .unwrap();

    // Create a test file that tries to use the alias
    let test_file = workspace_root.join("test.zen");
    std::fs::write(
        &test_file,
        r#"
# This should fail because the alias points to a nonexistent directory
MissingModule = Module("@missing/something.zen")
"#,
    )
    .unwrap();

    // Create an evaluation context with proper load resolver setup
    let ctx = create_eval_context(workspace_root);

    // Evaluate the test file
    let result = ctx
        .set_source_path(test_file.clone())
        .set_module_name("test".to_string())
        .set_inputs(InputMap::new())
        .eval();

    // Check that the evaluation failed with an appropriate error
    assert!(result.diagnostics.len() > 0, "Should have diagnostics");

    let diag_str = format!("{:?}", result.diagnostics);
    assert!(
        diag_str.contains("Failed to resolve")
            || diag_str.contains("does not exist")
            || diag_str.contains("No such file or directory"),
        "Error should mention resolution failure, got: {}",
        diag_str
    );
}
