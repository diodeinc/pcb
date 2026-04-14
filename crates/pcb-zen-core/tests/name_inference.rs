use pcb_zen_core::{DiagnosticsPass, SortPass};

mod common;

fn eval_ok(source: &str) -> pcb_zen_core::WithDiagnostics<pcb_zen_core::lang::eval::EvalOutput> {
    let mut result = common::eval_zen(vec![("test.zen".to_string(), source.to_string())]);
    SortPass.apply(&mut result.diagnostics);
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);
    result
}

fn redundancy_advice_count(diagnostics: &pcb_zen_core::Diagnostics, body_substring: &str) -> usize {
    diagnostics
        .iter()
        .filter(|diag| diag.body.contains(body_substring))
        .count()
}

#[test]
#[cfg(not(target_os = "windows"))]
fn infers_direct_net_names_from_assignment() {
    let result = eval_ok(
        r#"
Power = builtin.net_type("Power")

POWER = Net()
VDD = Power()

check(POWER.name == "POWER", "Net() should infer assigned variable name")
check(POWER.original_name == "POWER", "inferred Net() name should be canonical")
check(VDD.name == "VDD", "typed net should infer assigned variable name")
check(VDD.original_name == "VDD", "inferred typed net name should be canonical")
"#,
    );

    let warnings = result.diagnostics.warnings();
    assert!(
        warnings.is_empty(),
        "did not expect warnings for inferred direct net names, got: {:?}",
        warnings
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn infers_interface_root_for_generated_children_only() {
    let result = eval_ok(
        r#"
PowerIf = interface(vcc = Net, gnd = Net("GND"))
SystemIf = interface(power = PowerIf, data = Net)

EXT = Net()
EXTERNAL = PowerIf(vcc = EXT)
SYS = SystemIf(power = EXTERNAL)
AUTO = SystemIf()

check(EXTERNAL.vcc.name == "EXT", "provided net should keep its original name")
check(EXTERNAL.gnd.name == "EXTERNAL_GND", "generated child net should adopt assigned interface root")

check(SYS.power.vcc.name == "EXT", "provided nested net should not be renamed by outer interface")
check(SYS.power.gnd.name == "EXTERNAL_GND", "provided nested interface descendants should be preserved")
check(SYS.data.name == "SYS_data", "generated top-level child should adopt assigned interface root")

check(AUTO.power.vcc.name == "AUTO_power_vcc", "generated nested child should adopt full assigned root path")
check(AUTO.power.gnd.name == "AUTO_power_GND", "explicit leaf names should be preserved under inferred root path")
check(AUTO.data.name == "AUTO_data", "generated sibling child should adopt assigned interface root")
"#,
    );

    let warnings = result.diagnostics.warnings();
    assert!(
        warnings.is_empty(),
        "did not expect warnings for inferred interface names, got: {:?}",
        warnings
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn deduplicates_assignment_inferred_names_on_collision() {
    let result = eval_ok(
        r#"
Power = builtin.net_type("Power")

existing = Net("AUTO")
typed_existing = Power("VDD")
AUTO = Net()
VDD = Power()

check(AUTO.name == "AUTO_2", "inferred Net() name should deduplicate against explicit names")
check(AUTO.original_name == "AUTO", "deduplicated inferred Net() should preserve requested name")
check(VDD.name == "VDD_2", "inferred typed net name should deduplicate against explicit names")
check(VDD.original_name == "VDD", "deduplicated inferred typed net should preserve requested name")
"#,
    );

    let warnings = result.diagnostics.warnings();
    assert!(
        warnings
            .iter()
            .any(|warning| warning.body.contains("Net 'AUTO' was renamed to 'AUTO_2'")),
        "expected collision warning for inferred Net() name, got: {:?}",
        warnings
    );
    assert!(
        warnings
            .iter()
            .any(|warning| warning.body.contains("Net 'VDD' was renamed to 'VDD_2'")),
        "expected collision warning for inferred typed net name, got: {:?}",
        warnings
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn preserves_explicit_leaf_names_when_cloning_inferred_interface_templates() {
    let result = eval_ok(
        r#"
PowerIf = interface(vcc = Net("VCC"), gnd = Net("GND"))

TEMPLATE_PWR = PowerIf()
SystemIf = interface(power = TEMPLATE_PWR, data = Net("DATA"))
MAIN = SystemIf()

check(MAIN.power.vcc.name == "MAIN_power_VCC", "explicit nested leaf name should be preserved")
check(MAIN.power.gnd.name == "MAIN_power_GND", "explicit nested leaf name should be preserved")
check(MAIN.data.name == "MAIN_DATA", "explicit sibling leaf name should be preserved")
"#,
    );

    let warnings = result.diagnostics.warnings();
    assert!(
        warnings.is_empty(),
        "did not expect warnings for preserved explicit leaf names, got: {:?}",
        warnings
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn redundant_net_and_interface_names_emit_advice() {
    let result = eval_ok(
        r#"
load("@stdlib/interfaces.zen", "Analog", "I2c")

VCC = Net("VCC")
ANALOG = Analog("ANALOG")
BUS = I2c("BUS")
"#,
    );

    let net_advice = redundancy_advice_count(&result.diagnostics, "Net() name 'VCC' is redundant");
    let analog_advice =
        redundancy_advice_count(&result.diagnostics, "Net() name 'ANALOG' is redundant");
    let interface_advice =
        redundancy_advice_count(&result.diagnostics, "interface() name 'BUS' is redundant");
    assert_eq!(
        net_advice, 1,
        "expected one net redundancy advice, got: {:?}",
        result.diagnostics
    );
    assert_eq!(
        analog_advice, 1,
        "expected stdlib net types to use Net() redundancy advice, got: {:?}",
        result.diagnostics
    );
    assert_eq!(
        interface_advice, 1,
        "expected one interface redundancy advice, got: {:?}",
        result.diagnostics
    );
}
