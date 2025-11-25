mod common;
use common::TestProject;
use insta::assert_snapshot;

#[test]
fn test_physical_types() {
    let env = TestProject::new();

    env.add_file(
        "test_physical.zen",
        r#"
load("@stdlib/units.zen", "Voltage", "VoltageRange")

print("--- PhysicalValue ---")
# Test PhysicalValue.abs() exists and works
v1 = Voltage("-3.3V")
print("has abs:", hasattr(v1, "abs"))
print("abs value:", v1.abs())

print("\n--- PhysicalRange ---")
# Test PhysicalRange.abs() does NOT exist
r1 = VoltageRange("1V to 3V")
print("range has abs:", hasattr(r1, "abs"))

# We need to define a dummy module/component to satisfy the runner
Component(
    name = "Test",
    footprint = "test",
    symbol = Symbol(definition=[("1", ["1"])]),
    pins = {"1": Net("N1")}
)
        "#,
    );

    let result = env.eval_module("test_physical.zen");

    // Check for evaluation errors
    if result.output.is_none() {
        panic!("Evaluation failed: {:?}", result.diagnostics);
    }

    let output = result.output.unwrap();
    let stdout = output.print_output.join("\n");

    assert_snapshot!(stdout);
}
