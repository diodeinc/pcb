mod common;

use common::TestProject;

#[test]
fn test_physical_range_shim() {
    let env = TestProject::new();

    env.add_file(
        "test_physical_range_shim.zen",
        r#"
VoltageA = builtin.physical_value("V")
VoltageB = builtin.physical_range("V")

check(str(VoltageA) == str(VoltageB), "constructor types should have same display")

v1 = VoltageA("3.3V")
v2 = VoltageB("3.3V")
check(v1.unit == v2.unit, "constructed units should match")
check(v1.value == v2.value, "constructed values should match")

v = VoltageB("3.3V")
check(v.unit == "V", "expected V unit")
check(v.value == 3.3, "expected value 3.3")

r = VoltageB(min="1V", max="2V", nominal="1.5V")
check(r.min == 1.0, "expected min 1.0")
check(r.nominal == 1.5, "expected nominal 1.5")
check(r.max == 2.0, "expected max 2.0")

print("ok")
        "#,
    );

    let result = env.eval_module("test_physical_range_shim.zen");
    if result.output.is_none() {
        panic!("Evaluation failed: {:?}", result.diagnostics);
    }

    let stdout = result.output.unwrap().print_output.join("\n");
    assert!(stdout.contains("ok"));
}
