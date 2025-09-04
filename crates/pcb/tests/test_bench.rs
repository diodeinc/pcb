#![cfg(not(target_os = "windows"))]

use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

// Include test assets as strings
const MATCHERS_ZEN: &str = include_str!("assets/testbench/matchers.zen");
const SIMPLE_MODULE_ZEN: &str = include_str!("assets/testbench/simple_module.zen");
const SIMPLE_TESTBENCH_ZEN: &str = include_str!("assets/testbench/simple_testbench.zen");
const FACTORY_CHECKS_TESTBENCH_ZEN: &str =
    include_str!("assets/testbench/factory_checks_testbench.zen");
const FAILING_CHECKS_TESTBENCH_ZEN: &str =
    include_str!("assets/testbench/failing_checks_testbench.zen");

#[test]
fn test_simple_testbench() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.8"])
        .seed_kicad(&["9.0.0"])
        .write("matchers.zen", MATCHERS_ZEN)
        .write("simple_module.zen", SIMPLE_MODULE_ZEN)
        .write("simple_testbench.zen", SIMPLE_TESTBENCH_ZEN)
        .snapshot_run("pcb", ["test", "simple_testbench.zen"]);

    assert_snapshot!("simple_testbench", output);
}

#[test]
fn test_factory_pattern_checks() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.8"])
        .seed_kicad(&["9.0.0"])
        .write("matchers.zen", MATCHERS_ZEN)
        .write("simple_module.zen", SIMPLE_MODULE_ZEN)
        .write("factory_checks_testbench.zen", FACTORY_CHECKS_TESTBENCH_ZEN)
        .snapshot_run("pcb", ["test", "factory_checks_testbench.zen"]);

    assert_snapshot!("factory_pattern_checks", output);
}

#[test]
fn test_failing_checks() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.8"])
        .seed_kicad(&["9.0.0"])
        .write("matchers.zen", MATCHERS_ZEN)
        .write("simple_module.zen", SIMPLE_MODULE_ZEN)
        .write("failing_checks_testbench.zen", FAILING_CHECKS_TESTBENCH_ZEN)
        .snapshot_run("pcb", ["test", "failing_checks_testbench.zen"]);

    assert_snapshot!("failing_checks", output);
}

#[test]
fn test_json_output() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.8"])
        .seed_kicad(&["9.0.0"])
        .write("matchers.zen", MATCHERS_ZEN)
        .write("simple_module.zen", SIMPLE_MODULE_ZEN)
        .write("simple_testbench.zen", SIMPLE_TESTBENCH_ZEN)
        .snapshot_run("pcb", ["test", "simple_testbench.zen", "-f", "json"]);

    assert_snapshot!("json_output", output);
}

#[test]
fn test_tap_output() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.8"])
        .seed_kicad(&["9.0.0"])
        .write("matchers.zen", MATCHERS_ZEN)
        .write("simple_module.zen", SIMPLE_MODULE_ZEN)
        .write("simple_testbench.zen", SIMPLE_TESTBENCH_ZEN)
        .snapshot_run("pcb", ["test", "simple_testbench.zen", "-f", "tap"]);

    assert_snapshot!("tap_output", output);
}
