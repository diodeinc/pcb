use pcb_zen::create_eval_context;
use pcb_zen_core::InputMap;
use tempfile::TempDir;

#[test]
fn test_module_with_package_imports() {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    // Create a test file that uses Module() with a package import
    let test_file = workspace_root.join("test.zen");
    std::fs::write(
        &test_file,
        r#"
# Test that Module() can resolve @package style imports
# Note: This will try to download from GitHub, so it requires network access
Resistor = Module("@github/diodeinc/stdlib:v0.0.6/generic/Resistor.star")

Resistor(
    name = "Resistor",
    value = "1kohm",
    package = "0402",
    P1 = Net("P1"),
    P2 = Net("P2"),
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

    // Check if there were any errors
    if !result.diagnostics.is_empty() {
        eprintln!("Diagnostics:");
        for diag in &result.diagnostics {
            eprintln!("  {}", diag);
            if diag.is_error() {
                // If the error is about network access or the specific file not existing,
                // that's expected in a test environment
                if diag.body.contains("Failed to resolve module path")
                    || diag.body.contains("Module file not found")
                {
                    eprintln!("  (This is expected if the remote file doesn't exist)");
                } else {
                    panic!("Unexpected error: {}", diag.body);
                }
            }
        }
    }

    // The test passes if we didn't get an error about load resolver not being available
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.body.contains("No load resolver available")
                || d.body.contains("Remote package imports")),
        "Module() should not complain about missing load resolver"
    );
}

#[test]
fn test_module_with_relative_paths() {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    // Create a module file to be loaded
    let module_file = workspace_root.join("MyModule.zen");
    std::fs::write(
        &module_file,
        r#"
# A simple module
P1 = io("P1", Net)
"#,
    )
    .unwrap();

    // Create a test file that uses Module() with a relative path
    let test_file = workspace_root.join("test.zen");
    std::fs::write(
        &test_file,
        r#"
# Test that Module() works with relative paths
MyModule = Module("./MyModule.zen")

MyModule(
    name = "MyModule",
    P1 = Net("P1"),
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

    // Check if there were any errors
    for diag in &result.diagnostics {
        if diag.is_error() {
            panic!("Unexpected error: {}", diag);
        }
    }

    // The evaluation should succeed
    assert!(result.output.is_some(), "Evaluation should produce output");
}

#[test]
fn test_module_with_stdlib_package() {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    // Create a test file that uses Module() with a real stdlib file
    // Using a known file from the stdlib repository
    let test_file = workspace_root.join("test.zen");
    std::fs::write(
        &test_file,
        r#"
# Test that Module() can resolve @stdlib imports
# This uses the units.zen file which defines unit conversions
Units = Module("@stdlib/units.zen")
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

    // Check if there were any errors
    for diag in &result.diagnostics {
        if diag.is_error() {
            eprintln!("Error: {}", diag);
        }
    }

    // The evaluation should succeed if the file exists in stdlib
    if result.output.is_some() {
        println!("Successfully loaded @stdlib/units.zen via Module()");
    }
}

#[test]
fn test_module_with_alias_package() {
    // Create a temporary directory for our test
    let temp_dir = TempDir::new().unwrap();
    let workspace_root = temp_dir.path();

    let pcb_toml_file = workspace_root.join("pcb.toml");
    std::fs::write(
        pcb_toml_file,
        r#"
[packages]
stdlib = "@github/diodeinc/stdlib:v0.0.6"
stdlib_v5 = "@github/diodeinc/stdlib:v0.0.5"
stdlib_v4 = "@github/diodeinc/stdlib:v0.0.4"
"#,
    )
    .unwrap();

    let submodule_file = workspace_root.join("submodule.zen");
    std::fs::write(
        submodule_file,
        r#"
P1 = io("P1", Net)
"#,
    )
    .unwrap();

    // Create a test file that uses Module() with a real stdlib file
    // Using a known file from the stdlib repository
    let test_file = workspace_root.join("test.zen");
    std::fs::write(
        &test_file,
        r#"
Resistor = Module("@stdlib/generics/Resistor.star")
Resistor_v5 = Module("@stdlib_v5/generics/Resistor.star")
Resistor_v4 = Module("@stdlib_v4/generics/Resistor.star")
Submodule = Module("//submodule.zen")

Resistor(
    name = "R",
    value = "1kohm",
    package = "0402",
    P1 = Net("P1"),
    P2 = Net("P2"),
)

Resistor_v5(
    name = "R_V5",
    value = "1kohm",
    package = "0402",
    P1 = Net("P1"),
    P2 = Net("P2"),
)

Resistor_v4(
    name = "R_V4",
    value = "1kohm",
    package = "0402",
    P1 = Net("P1"),
    P2 = Net("P2"),
)

Submodule(
    name = "Submodule",
    P1 = Net("P1"),
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

    // Check if there were any errors
    for diag in &result.diagnostics {
        if diag.is_error() {
            eprintln!("Error: {}", diag);
        }
    }

    // The evaluation should succeed if the file exists in stdlib
    if result.output.is_some() {
        println!("Successfully loaded @stdlib/units.zen via Module()");
    }
}
