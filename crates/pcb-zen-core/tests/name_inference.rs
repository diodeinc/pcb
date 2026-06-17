use pcb_zen_core::lang::error::CategorizedDiagnostic;
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
        .filter(|diag| {
            diag.body.contains(body_substring)
                && diag
                    .downcast_error_ref::<CategorizedDiagnostic>()
                    .is_some_and(|c| c.kind == "style.redundant_name")
        })
        .count()
}

#[test]
#[cfg(not(target_os = "windows"))]
fn io_interface_template_preserves_borrowed_net_name() {
    let result = eval_ok(
        r#"
load("@stdlib/interfaces.zen", "Ground", "I2c")

GND = io(Ground())
I2C_TARGET = io(I2c(SDA=GND, SCL=GND), optional=True)

Component(
    name = "U1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": I2C_TARGET.SDA, "P2": GND},
    skip_bom = True,
)
"#,
    );

    let eval_output = result.output.expect("expected eval output");
    let sch_result = eval_output.to_schematic_with_diagnostics();
    assert!(
        !sch_result.diagnostics.has_errors(),
        "schematic conversion failed: {:?}",
        sch_result.diagnostics
    );

    let schematic = sch_result.output.expect("expected schematic");
    let mut ground_like_nets = schematic
        .nets
        .keys()
        .filter(|name| name.as_str() == "GND" || name.ends_with(".GND"))
        .cloned()
        .collect::<Vec<_>>();
    ground_like_nets.sort();

    assert_eq!(ground_like_nets, vec!["GND".to_string()]);
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
fn unassigned_regular_net_errors() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"Component(
    name = "U1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P1": "1"},
    pins = {"P1": Net()},
    skip_bom = True,
)"#
        .to_string(),
    )]);

    assert!(result.output.is_none(), "expected eval failure");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| diag.body == "Net is unnamed"),
        "expected unnamed net error, got: {:?}",
        result.diagnostics
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
fn rejects_assignment_inferred_name_collisions() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
Power = builtin.net_type("Power")

existing = Net("AUTO")
AUTO = Net()
"#
        .to_string(),
    )]);

    assert!(result.output.is_none(), "expected eval failure");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diag| diag.body.contains("Duplicate net name: AUTO")),
        "expected duplicate net name error, got: {:?}",
        result.diagnostics
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
fn inferred_templates_do_not_double_prefix() {
    let result = eval_ok(
        r#"
PowerIf = interface(vcc = Net, gnd = Net("GND"))

AUTO = PowerIf()
WrapperIf = interface(power = AUTO)
MAIN = WrapperIf()

check(AUTO.vcc.name == "AUTO_vcc", "generated leaf should infer from assigned root")
check(AUTO.gnd.name == "AUTO_GND", "explicit leaf should preserve its template leaf")
check(MAIN.power.vcc.name == "MAIN_power_vcc", "generated template leaf should not be double-prefixed")
check(MAIN.power.gnd.name == "MAIN_power_GND", "explicit template leaf should still be preserved")
"#,
    );

    let warnings = result.diagnostics.warnings();
    assert!(
        warnings.is_empty(),
        "did not expect warnings for cloned inferred interface templates, got: {:?}",
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
