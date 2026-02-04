mod common;
use common::TestProject;
use insta::assert_snapshot;

#[test]
fn test_physical_types() {
    let env = TestProject::new();

    env.add_file(
        "test_physical.zen",
        r#"
load("@stdlib/units.zen", "Voltage", "Frequency")

print("--- PhysicalValue ---")
# Test PhysicalValue.abs() exists and works
v1 = Voltage("-3.3V")
print("has abs:", hasattr(v1, "abs"))
print("abs value:", v1.abs())

print("\n--- Frequency Parsing ---")
# Test parsing of frequency with SI prefix and tolerance
f1 = Frequency("1MHz 0.1%")
print("Frequency:", f1)
print("Value:", f1.value)
print("Tolerance:", f1.tolerance)
print("Unit:", f1.unit)

print("\n--- Dimensionless ---")
# Test builtin.physical_value(None)
Dimensionless = builtin.physical_value(None)
print("Type object:", Dimensionless)

d = Dimensionless(1.5)
print("d:", d)
print("type(d):", type(d))
print("d.value:", d.value)
print("d.unit:", d.unit)

print("builtin.Voltage(1.5):", builtin.Voltage(1.5))

print("\n--- PhysicalValue with bounds ---")
r1 = Voltage("1V to 3V")
print("bounded has abs:", hasattr(r1, "abs"))
print("bounded min/max:", Voltage(min=11, max=26))
print("bounded with nominal:", Voltage(min=11, max=26, nominal=16))
print("bounded override nominal:", Voltage("11–26V", nominal="16V"))

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
