#[macro_use]
mod common;

use crate::common::{InMemoryFileProvider, eval_zen, stdlib_test_files, test_resolution};
use pcb_zen_core::lang::error::CategorizedDiagnostic;
use pcb_zen_core::lang::io_direction::IoDirection;
use pcb_zen_core::lang::net::FrozenNetValue;
use pcb_zen_core::{DiagnosticsPass, SortPass};
use starlark::values::ValueLike;
use std::path::PathBuf;
use std::sync::Arc;

fn redundancy_advice<'a>(
    diagnostics: &'a pcb_zen_core::Diagnostics,
    body_substring: &str,
) -> Vec<&'a pcb_zen_core::Diagnostic> {
    diagnostics
        .iter()
        .filter(|diag| {
            diag.body.contains(body_substring)
                && diag
                    .downcast_error_ref::<CategorizedDiagnostic>()
                    .is_some_and(|c| c.kind == "style.redundant_name")
        })
        .collect()
}

snapshot_eval!(config_default_implies_optional_in_signature, {
    "test.zen" => r#"
        # No explicit optional, but default is provided.
        led_color = config(str, default = "green")
    "#
});

snapshot_eval!(io_template_infers_signature_and_default, {
    "test.zen" => r#"
        Power = builtin.net_type("Power", voltage=Voltage)

        VDD = io(Power("VDD", voltage="3.3V"))
    "#
});

snapshot_eval!(io_template_enforces_voltage_compatibility, {
    "Module.zen" => r#"
        Power = builtin.net_type("Power", voltage=Voltage)

        VDD = io(Power("VDD", voltage="1.8V to 3.6V"))

        Component(
            name = "U1",
            footprint = "TEST:0402",
            pin_defs = {"VDD": "1"},
            pins = {"VDD": VDD},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        vdd = Mod.Power("VIN", voltage="5V")
        Mod(name = "child", VDD = vdd)
    "#
});

#[test]
fn io_rejects_template_positional_with_default() {
    let result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
        Power = builtin.net_type("Power", voltage=Voltage)

        VDD = io(
            Power("VDD", voltage="3.3V"),
            default=Power("ALT", voltage="3.3V"),
        )
    "#
        .to_string(),
    )]);

    assert!(
        !result.is_success(),
        "expected eval failure, got diagnostics: {:?}",
        result.diagnostics
    );
    assert!(
        result.diagnostics.iter().any(|diag| diag.body.contains(
            "io() cannot accept both a template positional argument and `default=`; remove `default=`"
        )),
        "expected ambiguous io() template/default diagnostic, got: {:?}",
        result.diagnostics
    );
}

snapshot_eval!(io_default_template_skips_implicit_checks, {
    "Module.zen" => r#"
        Power = builtin.net_type("Power", voltage=Voltage)

        VDD = io(Power, default=Power("VDD", voltage="1.8V to 3.6V"))

        Component(
            name = "U1",
            footprint = "TEST:0402",
            pin_defs = {"VDD": "1"},
            pins = {"VDD": VDD},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        vdd = Mod.Power("VIN", voltage="5V")
        Mod(name = "child", VDD = vdd)
    "#
});

#[test]
fn io_default_alias_template_reuses_net_identity() {
    let eval_result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                Power = builtin.net_type("Power", voltage=Voltage)

                VIN = io("VIN", Power)
                EN = io("EN", Net, optional=True, default=Net(VIN))

                Component(
                    name = "U1",
                    footprint = "TEST:0402",
                    pin_defs = {"VIN": "1", "EN": "2"},
                    pins = {"VIN": VIN, "EN": EN},
                )
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Mod = Module("Module.zen")

                vin = Mod.Power("VIN", voltage="12V")
                Mod(name = "child", VIN = vin)
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "default alias template should not duplicate net names: {:?}",
        eval_result.diagnostics
    );

    let output = eval_result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let child_module = module_tree
        .values()
        .find(|module| module.path().to_string() == "child")
        .expect("expected instantiated child module");
    let component = child_module
        .components()
        .find(|component| component.name() == "U1")
        .expect("expected child component");

    let vin = component
        .connections()
        .get("VIN")
        .and_then(|value| value.downcast_ref::<FrozenNetValue>())
        .expect("expected VIN net");
    let en = component
        .connections()
        .get("EN")
        .and_then(|value| value.downcast_ref::<FrozenNetValue>())
        .expect("expected EN net");

    assert_eq!(
        vin.id(),
        en.id(),
        "EN default should alias VIN, not clone it"
    );
    assert_eq!(vin.name(), "VIN");
    assert_eq!(en.name(), "VIN");
}

#[test]
fn io_interface_alias_clone_preserves_sharing() {
    let eval_result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                load("@stdlib/interfaces.zen", "I2c")
                Ground = builtin.net_type("Ground", voltage=field(Voltage, default=Voltage("0V")))

                GND = io("GND", Ground)
                I2C_TARGET = io(
                    "I2C_TARGET",
                    I2c,
                    optional=True,
                    default=I2c("I2C_TARGET", SDA=GND, SCL=GND),
                )

                Component(
                    name = "U1",
                    footprint = "TEST:0402",
                    pin_defs = {"SDA": "1", "SCL": "2"},
                    pins = {"SDA": I2C_TARGET.SDA, "SCL": I2C_TARGET.SCL},
                )
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Ground = builtin.net_type("Ground", voltage=field(Voltage, default=Voltage("0V")))
                Mod = Module("Module.zen")
                Mod(name = "child", GND = Ground("GND"))
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "interface alias template should not duplicate leaf net names: {:?}",
        eval_result.diagnostics
    );
    assert!(
        eval_result
            .diagnostics
            .warnings()
            .iter()
            .all(|diag| !diag.body.contains("I2C_TARGET_GND")),
        "did not expect I2C_TARGET_GND rename warning, got: {:?}",
        eval_result.diagnostics
    );

    let output = eval_result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let child_module = module_tree
        .values()
        .find(|module| module.path().to_string() == "child")
        .expect("expected instantiated child module");
    let gnd = child_module
        .inputs()
        .get("GND")
        .and_then(|value| value.downcast_ref::<FrozenNetValue>())
        .expect("expected child GND input");
    let component = child_module
        .components()
        .find(|component| component.name() == "U1")
        .expect("expected child component");

    let sda = component
        .connections()
        .get("SDA")
        .and_then(|value| value.downcast_ref::<FrozenNetValue>())
        .expect("expected SDA net");
    let scl = component
        .connections()
        .get("SCL")
        .and_then(|value| value.downcast_ref::<FrozenNetValue>())
        .expect("expected SCL net");

    assert_eq!(
        sda.id(),
        scl.id(),
        "SDA and SCL should alias the same GND net"
    );
    assert_ne!(
        sda.id(),
        gnd.id(),
        "interface defaults should clone shared leaf nets, not reuse the source input net"
    );
    assert_eq!(sda.name(), "I2C_TARGET_GND");
    assert_eq!(scl.name(), "I2C_TARGET_GND");
}

#[test]
fn io_template_implicit_check_warning_preserves_child_instantiation() {
    let eval_result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                Power = builtin.net_type("Power", voltage=Voltage)

                VDD = io(Power("VDD", voltage="1.8V to 3.6V"))

                Component(
                    name = "U1",
                    footprint = "TEST:0402",
                    pin_defs = {"VDD": "1"},
                    pins = {"VDD": VDD},
                )
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Mod = Module("Module.zen")

                vdd = Mod.Power("VIN", voltage="5V")
                Mod(name = "child", VDD = vdd)
            "#
            .to_string(),
        ),
    ]);

    assert!(
        eval_result.output.is_some(),
        "implicit-check failures should not abort eval output: {:?}",
        eval_result.diagnostics
    );
    assert!(
        !eval_result.diagnostics.has_errors(),
        "implicit-check failures should now warn without becoming errors: {:?}",
        eval_result.diagnostics
    );
    assert!(
        !eval_result.diagnostics.warnings().is_empty(),
        "expected warning diagnostic for implicit-check failure"
    );
    assert!(
        format!("{:?}", eval_result.diagnostics)
            .contains("Input 'VDD' voltage 5V is not within template voltage"),
        "expected implicit-check warning, got: {:?}",
        eval_result.diagnostics
    );

    let output = eval_result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let child_module = module_tree
        .values()
        .find(|module| module.path().to_string() == "child")
        .expect("expected instantiated child module");
    let component = child_module
        .components()
        .find(|component| component.name() == "U1")
        .expect("expected child component despite implicit-check error");
    let net = component
        .connections()
        .get("VDD")
        .and_then(|value| value.downcast_ref::<FrozenNetValue>())
        .expect("expected connected VDD net");

    assert_eq!(net.name(), "VIN");
}

#[test]
fn io_generated_net_warning_uses_io_declaration_span() {
    let eval_result = eval_zen(vec![
        (
            "power_pin.kicad_sym".to_string(),
            r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "PowerPin"
    (property "Reference" "U")
    (symbol "PowerPin_0_1"
      (pin power_in line
        (at 0 0 0)
        (length 2.54)
        (name "VCC")
        (number "1")
      )
    )
  )
)"#
            .to_string(),
        ),
        (
            "test.zen".to_string(),
            r#"
        symbol = Symbol(library = "power_pin.kicad_sym")

        VDD = io(Net())

        Component(
            name = "U1",
            footprint = "TEST:0402",
            symbol = symbol,
            pins = {
                "VCC": VDD,
            },
        )
    "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let warnings = eval_result.diagnostics.warnings();
    let warning = warnings
        .iter()
        .find(|diag| {
            diag.downcast_error_ref::<CategorizedDiagnostic>()
                .is_some_and(|c| c.kind == "pin.power_net")
        })
        .expect("expected pin.power_net warning");

    assert_eq!(warning.path, "test.zen");
    assert!(
        warning.span.is_some(),
        "expected pin.power_net warning to have an io() declaration span, got: {:?}",
        warning
    );
}

snapshot_eval!(config_optional_false_missing_emits_error_diagnostic, {
    "Module.zen" => r#"
        led_color = config(str, default = "green", optional = False)

        Component(
            name = "D1",
            footprint = "TEST:0402",
            pin_defs = {"A": "1", "K": "2"},
            pins = {"A": Net("VCC"), "K": Net("GND")},
            properties = {"color": led_color},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")
        Mod(name = "U1")
    "#
});

snapshot_eval!(io_config, {
    "Module.zen" => r#"
        pwr = io(Net)
        baud = config(int)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": pwr},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "U1",
            pwr = Net("VCC"),
            baud = 9600,
        )
    "#
});

snapshot_eval!(missing_required_io_config, {
    "Module.zen" => r#"
        pwr = io(Net)
        baud = config(int)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": pwr},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "U1",
            # intentionally omit `pwr` and `baud` - should trigger an error
        )
    "#
});

snapshot_eval!(optional_io_config, {
    "Module.zen" => r#"
        pwr = io(Net, optional = True)
        baud = config(int, optional = True)

        # The io() should be default-initialized, and the config() should be None.
        check(pwr != None, "pwr should not be None when omitted")
        check(baud == None, "baud should be None when omitted")

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = {"V": "1"},
            pins = {"V": Net("INTERNAL_V")},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "U1",
            # omit both inputs - allowed because they are optional
        )
    "#
});

snapshot_eval!(interface_io, {
    "Module.zen" => r#"
        Power = interface(vcc = Net)
        PdmMic = interface(power = Power, data = Net, select = Net, clock = Net)

        pdm = io(PdmMic)
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        pdm = Mod.PdmMic("PDM")
        Mod(name = "U1", pdm = pdm)
    "#
});

#[test]
fn config_named_checks() {
    let eval_result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                def nonnegative(value):
                    check(value >= 0, "value must be nonnegative")

                value = config(int, checks = nonnegative)
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Mod = Module("Module.zen")
                Mod(name = "U1", value = 1)
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn io_named_checks() {
    let eval_result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                def present(net):
                    check(net != None, "net must be present")

                pwr = io(Net, checks = present)

                Component(
                    name = "comp0",
                    footprint = "TEST:0402",
                    pin_defs = {"V": "1"},
                    pins = {"V": pwr},
                )
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Mod = Module("Module.zen")
                Mod(name = "U1", pwr = Net("VCC"))
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn config_positional_none_checks_is_ignored() {
    let eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            value = config(int, None)
            check(value == 0, "config() should still resolve with positional None checks")
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn io_positional_none_checks_is_ignored() {
    let eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            VIN = io(Net, None, optional = True)
            check(VIN != None, "io() should still resolve with positional None checks")
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn config_name_infers_from_assignment() {
    let eval_result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                description = config(str)
                skip_bom = config(bool, default = True)

                Component(
                    name = "R1",
                    footprint = "TEST:0402",
                    pin_defs = {"P": "1"},
                    pins = {"P": Net("SIG")},
                    description = description,
                    skip_bom = skip_bom,
                )
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Mod = Module("Module.zen")
                Mod(name = "U1", description = "Acme")
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let output = eval_result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let child_module = module_tree
        .values()
        .find(|module| module.path().to_string() == "U1")
        .expect("expected instantiated child module");
    let component = child_module
        .components()
        .find(|component| component.name() == "R1")
        .expect("expected child component");

    assert_eq!(component.description(), Some("Acme"));
    assert!(
        component.skip_bom(),
        "defaulted inferred config should still apply"
    );
}

#[test]
fn inferred_config_values_work_in_component_kwargs() {
    let eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            manufacturer = config(str, default = "Acme")
            skip_bom = config(bool, default = True)

            Component(
                name = "R1",
                footprint = "TEST:0402",
                pin_defs = {"P": "1"},
                pins = {"P": Net("SIG")},
                manufacturer = manufacturer,
                skip_bom = skip_bom,
            )
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn unused_nameless_config_is_ignored() {
    let eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            config(int)
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "unused nameless config() should be ignored, got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn io_name_infers_from_assignment() {
    let eval_result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                SIG = io(Net)

                Component(
                    name = "R1",
                    footprint = "TEST:0402",
                    pin_defs = {"P": "1"},
                    pins = {"P": SIG},
                )
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Mod = Module("Module.zen")
                Mod(name = "U1", SIG = Net("INPUT"))
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let output = eval_result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let child_module = module_tree
        .values()
        .find(|module| module.path().to_string() == "U1")
        .expect("expected instantiated child module");
    let component = child_module
        .components()
        .find(|component| component.name() == "R1")
        .expect("expected child component");
    let net = component
        .connections()
        .get("P")
        .and_then(|value| value.downcast_ref::<FrozenNetValue>())
        .expect("expected connected net");

    assert_eq!(net.name(), "INPUT");
}

#[test]
fn unused_nameless_io_is_ignored() {
    let eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            io(Net)
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "unused nameless io() should be ignored, got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn net_cast_from_inferred_net_preserves_runtime_name_without_setting_original_name() {
    let eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            AUTO = Net()
            COPY = Net(AUTO)
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let output = eval_result.output.expect("expected eval output");
    let copy = output.star_module.get("COPY").expect("expected COPY");
    let net = copy
        .value()
        .downcast_ref::<FrozenNetValue>()
        .expect("expected COPY to be a net");

    assert_eq!(net.name(), "AUTO");
    assert_eq!(
        net.original_name_opt(),
        None,
        "casting an inferred net should not invent an explicit original name"
    );
}

#[test]
fn redundant_io_and_config_names_emit_advice() {
    let mut eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            VIN = io("VIN", Net)
            manufacturer = config("manufacturer", str, default = "Acme")
        "#
        .to_string(),
    )]);
    pcb_zen_core::SortPass.apply(&mut eval_result.diagnostics);

    let config_advice = redundancy_advice(
        &eval_result.diagnostics,
        "config() name 'manufacturer' is redundant",
    );
    let io_advice = redundancy_advice(&eval_result.diagnostics, "io() name 'VIN' is redundant");

    assert_eq!(
        config_advice.len(),
        1,
        "unexpected diagnostics: {:?}",
        eval_result.diagnostics
    );
    assert_eq!(
        io_advice.len(),
        1,
        "unexpected diagnostics: {:?}",
        eval_result.diagnostics
    );
    assert_eq!(config_advice[0].path, "Module.zen");
    assert_eq!(io_advice[0].path, "Module.zen");
    assert!(
        config_advice[0].span.is_some() && io_advice[0].span.is_some(),
        "expected source spans for advice"
    );
}

#[test]
fn explicit_names_emit_style_advice_without_assignment() {
    let mut eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            load("@stdlib/interfaces.zen", "I2c")

            io("signal", Net)
            config("ClockRate", int, default = 100)
            Net("vcc")
            I2c("bus")
        "#
        .to_string(),
    )]);
    pcb_zen_core::SortPass.apply(&mut eval_result.diagnostics);

    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("io() name 'signal' should be UPPERCASE: 'SIGNAL'")),
        "expected standalone io() explicit-name advice, got: {:?}",
        eval_result.diagnostics
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("config() name 'ClockRate' should be snake_case: 'clock_rate'")),
        "expected standalone config() explicit-name advice, got: {:?}",
        eval_result.diagnostics
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("Net() name 'vcc' should be UPPERCASE: 'VCC'")),
        "expected standalone Net() explicit-name advice, got: {:?}",
        eval_result.diagnostics
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("interface() name 'bus' should be UPPERCASE: 'BUS'")),
        "expected standalone interface() explicit-name advice, got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn assignment_and_explicit_name_checks_are_independent() {
    let mut eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            pwr = io("signal", Net)
            BaudRate = config("ClockRate", int, default = 100)
        "#
        .to_string(),
    )]);
    pcb_zen_core::SortPass.apply(&mut eval_result.diagnostics);

    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("io() parameter 'pwr' should be UPPERCASE: 'PWR'")),
        "expected assignment-name io() advice, got: {:?}",
        eval_result.diagnostics
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("io() name 'signal' should be UPPERCASE: 'SIGNAL'")),
        "expected explicit-name io() advice, got: {:?}",
        eval_result.diagnostics
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("config() parameter 'BaudRate' should be snake_case: 'baud_rate'")),
        "expected assignment-name config() advice, got: {:?}",
        eval_result.diagnostics
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("config() name 'ClockRate' should be snake_case: 'clock_rate'")),
        "expected explicit-name config() advice, got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn explicit_param_names_do_not_warn_when_assignment_differs() {
    let mut eval_result = eval_zen(vec![(
        "Module.zen".to_string(),
        r#"
            rail = io("VCC", Power)
            package_name = config("package", str, default = "0603")
            PowerIf = interface(vcc = Net, gnd = Net)
            net_alias = Net("VIN")
            bus = PowerIf("BUS")
        "#
        .to_string(),
    )]);
    pcb_zen_core::SortPass.apply(&mut eval_result.diagnostics);

    let redundancy_advice = eval_result.diagnostics.iter().filter(|diag| {
        diag.downcast_error_ref::<CategorizedDiagnostic>()
            .is_some_and(|c| c.kind == "style.redundant_name")
    });

    assert_eq!(
        redundancy_advice.count(),
        0,
        "did not expect redundancy advice, got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn unused_io_warns_only_for_unconnected_ports() {
    let eval_result = eval_zen(vec![
        (
            "Leaf.zen".to_string(),
            r#"
                VIN = io(Net)

                Component(
                    name = "LOAD",
                    footprint = "TEST:0402",
                    pin_defs = {"P": "1"},
                    pins = {"P": VIN},
                )
            "#
            .to_string(),
        ),
        (
            "Wrapper.zen".to_string(),
            r#"
                Leaf = Module("Leaf.zen")

                Bus = interface(DATA = Net, CTRL = Net)

                VIN = io(Net)
                SPARE = io(Net)
                BUS = io(Bus)
                UNUSED_BUS = io(Bus)

                Component(
                    name = "TAP",
                    footprint = "TEST:0402",
                    pin_defs = {"P": "1"},
                    pins = {"P": BUS.DATA},
                )

                Leaf(name = "LEAF", VIN = VIN)
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Wrapper = Module("Wrapper.zen")

                bus = Wrapper.Bus("BUS")
                unused_bus = Wrapper.Bus("UNUSED")

                Wrapper(
                    name = "WRAP",
                    VIN = Net("VIN"),
                    SPARE = Net("SPARE"),
                    BUS = bus,
                    UNUSED_BUS = unused_bus,
                )
            "#
            .to_string(),
        ),
    ]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let eval_output = eval_result.output.expect("expected eval output");
    let sch_result = eval_output.to_schematic_with_diagnostics();

    assert!(
        !sch_result.diagnostics.has_errors(),
        "schematic conversion produced unexpected errors: {:?}",
        sch_result.diagnostics
    );

    let unused_io_bodies: Vec<String> = sch_result
        .diagnostics
        .iter()
        .filter(|diag| {
            diag.downcast_error_ref::<CategorizedDiagnostic>()
                .map(|categorized| categorized.kind == "module.io.unused")
                .unwrap_or(false)
        })
        .map(|diag| diag.body.clone())
        .collect();
    let unused_io_paths: Vec<String> = sch_result
        .diagnostics
        .iter()
        .filter(|diag| {
            diag.downcast_error_ref::<CategorizedDiagnostic>()
                .map(|categorized| categorized.kind == "module.io.unused")
                .unwrap_or(false)
        })
        .map(|diag| diag.path.clone())
        .collect();

    assert_eq!(
        unused_io_bodies.len(),
        2,
        "unexpected warnings: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies
            .iter()
            .any(|body| body.contains("SPARE") && body.contains("WRAP")),
        "missing SPARE warning: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies
            .iter()
            .any(|body| body.contains("UNUSED_BUS") && body.contains("WRAP")),
        "missing UNUSED_BUS warning: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies.iter().all(|body| !body.contains("VIN")),
        "forwarded VIN should not warn: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_bodies
            .iter()
            .all(|body| !body.starts_with("io() 'BUS'")),
        "partially used BUS interface should not warn: {unused_io_bodies:?}"
    );
    assert!(
        unused_io_paths
            .iter()
            .all(|path| path.ends_with("Wrapper.zen") && !path.contains("/stdlib/io.zen")),
        "unused io warnings should point at Wrapper.zen, got: {unused_io_paths:?}"
    );
}

snapshot_eval!(io_interface_incompatible, {
    "Module.zen" => r#"
        signal = io(Net)
    "#,
    "parent.zen" => r#"
        Mod = Module("Module.zen")

        SingleNet = interface(signal = Net)
        sig_if = SingleNet("SIG")

        Mod(name="U1", signal=sig_if)  # Should fail - interface not accepted for Net io
    "#
});

snapshot_eval!(config_str, {
    "test.zen" => r#"
        value = config(str)

        # Use the string config
        Component(
            name = "test_comp",
            footprint = "test_footprint",
            pin_defs = {"in": "1", "out": "2"},
            pins = {
                "in": Net("1"),
                "out": Net("2")
            },
            properties = {
                "value": value
            }
        )
    "#
});

snapshot_eval!(config_types, {
    "test.zen" => r#"
        # Test various config() and io() declarations for signature generation

        # Basic types
        str_config = config(str)
        int_config = config(int)
        float_config = config(float)
        bool_config = config(bool)

        # Optional configs with defaults
        opt_str = config(str, optional=True, default="default_value")
        opt_int = config(int, optional=True, default=42)
        opt_float = config(float, optional=True, default=3.14)
        opt_bool = config(bool, optional=True, default=True)

        # Optional without defaults
        opt_no_default = config(str, optional=True)

        # IO declarations
        net_io = io(Net)
        opt_net_io = io(Net, optional=True)

        # Interface types
        Power = interface(vcc = Net, gnd = Net)
        power_io = io(Power)
        opt_power_io = io(Power, optional=True)

        # Nested interface
        DataBus = interface(
            data = Net,
            clock = Net,
            enable = Net
        )
        bus_io = io(DataBus)

        # Complex nested interface
        System = interface(
            power = Power,
            bus = DataBus,
            reset = Net
        )
        system_io = io(System)

        # Add a simple component to make the module valid
        Component(
            name = "test",
            type = "test_component",
            pin_defs = {"1": "1"},
            footprint = "TEST:FP",
            pins = {"1": Net("TEST")},
        )
    "#
});

snapshot_eval!(implicit_enum_conversion, {
    "Module.zen" => r#"
        Direction = enum("NORTH", "SOUTH")

        heading = config(Direction)

        Component(
            name = "comp0",
            footprint = "TEST:0402",
            pin_defs = { "V": "1" },
            pins = { "V": Net("VCC") },
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(
            name = "child",
            heading = "NORTH",
        )
    "#
});

snapshot_eval!(interface_net_incompatible, {
    "Module.zen" => r#"
        SingleNet = interface(signal = Net)

        signal_if = SingleNet(name="sig")

        Component(
            name = "test_comp",
            footprint = "test_footprint",
            pin_defs = {"in": "1", "out": "2"},
            pins = {
                "in": signal_if,  # This should fail - interfaces not accepted for pins
                "out": Net()
            }
        )
    "#
});

snapshot_eval!(interface_net_template_basic, {
    "Module.zen" => r#"
        MyInterface = interface(test = Net("MYTEST"))
        instance = MyInterface("PREFIX")

        Component(
            name = "R1",
            type = "resistor",
            pin_defs = {"1": "1", "2": "2"},
            footprint = "SMD:0805",
            pins = {"1": instance.test, "2": Net("GND")},
        )
    "#
});

snapshot_eval!(interface_multiple_net_templates, {
    "test.zen" => r#"
        Power = interface(
            vcc = Net("3V3"),
            gnd = Net("GND"),
            enable = Net("EN")
        )

        pwr1 = Power("MCU")
        pwr2 = Power("SENSOR")

        Component(
            name = "U1",
            type = "mcu",
            pin_defs = {"VCC": "1", "GND": "2", "EN": "3"},
            footprint = "QFN:32",
            pins = {
                "VCC": pwr1.vcc,
                "GND": pwr1.gnd,
                "EN": pwr1.enable,
            }
        )

        Component(
            name = "U2",
            type = "sensor",
            pin_defs = {"VDD": "1", "VSS": "2", "ENABLE": "3"},
            footprint = "SOT:23-6",
            pins = {
                "VDD": pwr2.vcc,
                "VSS": pwr2.gnd,
                "ENABLE": pwr2.enable,
            }
        )
    "#
});

snapshot_eval!(interface_nested_template, {
    "test.zen" => r#"
        # Nested interface templates
        PowerNets = interface(
            vcc = Net("VCC"),
            gnd = Net("GND")
        )

        # Create a pre-configured power instance
        usb_power = PowerNets("USB")

        # Use as template in another interface
        Device = interface(
            power = usb_power,
            data_p = Net("D+"),
            data_n = Net("D-")
        )

        # Create device instance
        dev = Device("PORT1")

        # Wire up components
        Component(
            name = "J1",
            type = "usb_connector",
            pin_defs = {"VBUS": "1", "D+": "2", "D-": "3", "GND": "4"},
            footprint = "USB:TYPE-C",
            pins = {
                "VBUS": dev.power.vcc,
                "D+": dev.data_p,
                "D-": dev.data_n,
                "GND": dev.power.gnd,
            }
        )
    "#
});

snapshot_eval!(interface_mixed_templates_and_types, {
    "test.zen" => r#"
        # Mix of templates and regular types
        MixedInterface = interface(
            # Template nets without properties
            power = Net("VDD"),
            ground = Net("VSS"),
            # Regular net type
            signal = Net,
            # Nested interface template
            control = interface(
                enable = Net("EN"),
                reset = Net("RST")
            )()
        )

        # Create instance
        mixed = MixedInterface("CHIP")

        # Use all the nets
        Component(
            name = "IC1",
            type = "asic",
            pin_defs = {
                "VDD": "1",
                "VSS": "2",
                "SIG": "3",
                "EN": "4",
                "RST": "5"
            },
            footprint = "QFN:48",
            pins = {
                "VDD": mixed.power,
                "VSS": mixed.ground,
                "SIG": mixed.signal,
                "EN": mixed.control.enable,
                "RST": mixed.control.reset,
            }
        )
    "#
});

snapshot_eval!(config_without_convert_fails_type_check, {
    "Module.zen" => r#"
        UnitType = record(
            value = field(float),
            unit = field(str),
        )

        # This should fail because "5V" is not a record and no converter is provided
        # Provide a default since records require defaults
        voltage = config(UnitType, default = UnitType(value = 0.0, unit = "V"))
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        # This should fail - string cannot be used for record type without converter
        Mod(
            name = "test",
            voltage = "5V",
        )
    "#
});

snapshot_eval!(io_config_with_help_text, {
    "Module.zen" => r#"
        # Test io() and config() with help parameter
        
        # IO with help text
        power = io(Net, help = "Main power supply net")
        data = io(Net, optional = True, help = "Optional data line")
        
        # Config with help text
        baud_rate = config(int, default = 9600, help = "Serial communication baud rate")
        device_name = config(str, help = "Human-readable device identifier")
        
        # Optional config with help
        debug_mode = config(bool, optional = True, help = "Enable debug logging")
        
        voltage = config(float, default = 3.3, help = "Operating voltage in volts")
        
        # Add a component to make the module valid
        Component(
            name = "test",
            footprint = "TEST:0402",
            pin_defs = {"PWR": "1", "GND": "2"},
            pins = {"PWR": power, "GND": Net("GND")},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")
        
        # Create module instance with some parameters
        Mod(
            name = "U1",
            power = Net("VCC"),
            baud_rate = 115200,
            device_name = "TestDevice",
            voltage = 5.0,
        )
    "#
});

snapshot_eval!(cfg_enum_value, {
    "Module.zen" => r#"
        # Test io() with enum value

        EnumType = enum("A", "B", "C")
        
        cfg = config(EnumType, default = "A")
        print(cfg)
    "#,
    "top.zen" => r#"
        MyModule = Module("./Module.zen")

        # Create module instance with some parameters
        MyModule(
            name = "U1",
            cfg = MyModule.EnumType("B"),
        )
    "#
});

snapshot_eval!(config_int_to_float_conversion, {
    "Module.zen" => r#"
        # Test automatic int to float conversion
        voltage = config(float)
        current = config(float, default = 1)  # int default should convert to float
        power = config(float, optional = True)
        
        # Verify the values are floats
        add_property("voltage_value", voltage)
        add_property("voltage_type", type(voltage))
        add_property("current_value", current) 
        add_property("current_type", type(current))
        
        # Test arithmetic to ensure they behave as floats
        add_property("voltage_divided", voltage / 2)
        add_property("current_multiplied", current * 1.5)
        
        # Optional power should be None when not provided
        add_property("power_is_none", power == None)
    "#,
    "top.zen" => r#"
        MyModule = Module("./Module.zen")
        
        # Provide integer values that should be converted to floats
        m = MyModule(
            name = "test",
            voltage = 5,      # int 5 should become float 5.0
            current = 2,      # int 2 should become float 2.0
            # power is not provided, should be None
        )
    "#
});

snapshot_eval!(config_mixed_numeric_types, {
    "Module.zen" => r#"
        # Test that float values remain floats and int values convert to float
        voltage1 = config(float)
        voltage2 = config(float) 
        voltage3 = config(float, default = 0)  # int default
        
        # Verify all are floats
        add_property("v1_value", voltage1)
        add_property("v1_type", type(voltage1))
        add_property("v2_value", voltage2)
        add_property("v2_type", type(voltage2))
        add_property("v3_value", voltage3)
        add_property("v3_type", type(voltage3))
        
        # Test that float arithmetic works correctly
        add_property("sum", voltage1 + voltage2 + voltage3)
    "#,
    "top.zen" => r#"
        MyModule = Module("./Module.zen")
        
        m = MyModule(
            name = "test",
            voltage1 = 3.14,   # Already a float
            voltage2 = 10,     # Int that should convert to float
            # voltage3 uses default int 0 that should convert to float
        )
    "#
});

snapshot_eval!(io_invalid_type, {
    "test.zen" => r#"
        # io() should only accept NetType or InterfaceFactory, not primitive types
        value = io(int)
    "#
});

snapshot_eval!(config_string_to_physical_value, {
    "child.zen" => r#"
        voltage = config(builtin.physical_value("V"))
        resistance = config(builtin.physical_value("Ω"))
        current = config(builtin.physical_value("A"))

        print("voltage:", voltage)
        print("resistance:", resistance)
        print("current:", current)
    "#,
    "test.zen" => r#"
        Child = Module("child.zen")

        # Provide mixed scalar/string values that should be converted
        # through the PhysicalValue constructor path.
        Child(name = "test", voltage = "3.3V", resistance = 10000, current = 0.02)

        print("String to PhysicalValue conversion: success")
    "#
});

snapshot_eval!(config_string_to_physical_value_with_bounds, {
    "child.zen" => r#"
        voltage = config(builtin.physical_value("V"))

        print("voltage:", voltage)
    "#,
    "test.zen" => r#"
        Child = Module("child.zen")

        Child(name = "test", voltage = "3.0V to 3.6V")

        print("String to PhysicalValue (bounds) conversion: success")
    "#
});

#[test]
fn io_direction_appears_in_signature() {
    let eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            VIN = io(Net, direction = "input")
            VOUT = io(Net, direction = "output")
            BIDIR = io(Net)

            Component(
                name = "test",
                footprint = "TEST:0402",
                pin_defs = {"IN": "1", "OUT": "2", "IO": "3"},
                pins = {"IN": VIN, "OUT": VOUT, "IO": BIDIR},
            )
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let eval_output = eval_result.output.expect("expected eval output");
    let signature = eval_output.signature;

    let vin = signature
        .iter()
        .find(|param| param.name == "VIN")
        .expect("expected VIN in signature");
    assert_eq!(vin.direction, Some(IoDirection::Input));

    let vout = signature
        .iter()
        .find(|param| param.name == "VOUT")
        .expect("expected VOUT in signature");
    assert_eq!(vout.direction, Some(IoDirection::Output));

    let bidir = signature
        .iter()
        .find(|param| param.name == "BIDIR")
        .expect("expected BIDIR in signature");
    assert_eq!(bidir.direction, None);
}

#[test]
fn io_direction_rejects_invalid_values() {
    let eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            VIN = io(Net, direction = "in")
        "#
        .to_string(),
    )]);

    assert!(
        eval_result.output.is_none(),
        "expected evaluation to fail for invalid direction"
    );
    assert!(
        eval_result.diagnostics.iter().any(|diag| diag
            .body
            .contains("io() direction must be \"input\" or \"output\"")),
        "expected invalid direction diagnostic, got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn input_output_set_direction_metadata() {
    let eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            VIN = input(Net)
            VOUT = output(Net, help = "Output net")

            Component(
                name = "test",
                footprint = "TEST:0402",
                pin_defs = {"IN": "1", "OUT": "2"},
                pins = {"IN": VIN, "OUT": VOUT},
            )
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );

    let eval_output = eval_result.output.expect("expected eval output");
    let signature = eval_output.signature;

    let vin = signature
        .iter()
        .find(|param| param.name == "VIN")
        .expect("expected VIN in signature");
    assert_eq!(vin.direction, Some(IoDirection::Input));

    let vout = signature
        .iter()
        .find(|param| param.name == "VOUT")
        .expect("expected VOUT in signature");
    assert_eq!(vout.direction, Some(IoDirection::Output));
    assert_eq!(vout.help.as_deref(), Some("Output net"));
}

#[test]
fn builtin_io_available_directly() {
    let eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            VIN = builtin.io("VIN", Net, direction = "input")
            VOUT = builtin.io("VOUT", Net)

            Component(
                name = "test",
                footprint = "TEST:0402",
                pin_defs = {"IN": "1", "OUT": "2"},
                pins = {"IN": VIN, "OUT": VOUT},
            )
        "#
        .to_string(),
    )]);

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn builtin_io_skips_ast_style_lints() {
    let mut eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            signal = builtin.io("signal", Net)
        "#
        .to_string(),
    )]);
    SortPass.apply(&mut eval_result.diagnostics);

    assert!(
        redundancy_advice(&eval_result.diagnostics, "io() name 'signal' is redundant").is_empty(),
        "did not expect redundant-name advice for dotted builtin.io(), got: {:?}",
        eval_result.diagnostics
    );
    let io_advice_count = eval_result
        .diagnostics
        .iter()
        .filter(|diag| diag.body.contains("io() parameter 'signal'"))
        .count();
    assert_eq!(
        io_advice_count, 0,
        "did not expect naming advice for dotted builtin.io(), got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn stdlib_interface_io_only_lints_root_name() {
    let mut eval_result = eval_zen(vec![(
        "test.zen".to_string(),
        r#"
            load("@stdlib/interfaces.zen", "Usb2")

            usb = io(Usb2)
        "#
        .to_string(),
    )]);
    SortPass.apply(&mut eval_result.diagnostics);

    let root_advice: Vec<_> = eval_result
        .diagnostics
        .iter()
        .filter(|diag| {
            diag.body
                .contains("io() parameter 'usb' should be UPPERCASE: 'USB'")
        })
        .collect();
    let generated_net_advice: Vec<_> = eval_result
        .diagnostics
        .iter()
        .filter(|diag| diag.body.contains("Net name 'usb_"))
        .collect();

    assert_eq!(
        root_advice.len(),
        1,
        "expected one root io() naming advice for direct io(Usb2), got: {:?}",
        eval_result.diagnostics
    );
    assert!(
        generated_net_advice.is_empty(),
        "did not expect generated child-net naming advice for stdlib interfaces, got: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn prelude_injects_io_helpers_from_stdlib() {
    let mut files = stdlib_test_files();
    files.insert(
        "test.zen".to_string(),
        r#"
            VIN = input(Power)
            VOUT = output(Net)
            GND = io(Ground)

            Component(
                name = "test",
                footprint = "TEST:0402",
                pin_defs = {"IN": "1", "OUT": "2", "G": "3"},
                pins = {"IN": VIN, "OUT": VOUT, "G": GND},
            )
        "#
        .to_string(),
    );

    let file_provider: Arc<dyn pcb_zen_core::FileProvider> =
        Arc::new(InMemoryFileProvider::new(files));
    let eval_result = pcb_zen_core::EvalContext::new(file_provider, test_resolution())
        .set_source_path(PathBuf::from("test.zen"))
        .eval();

    assert!(
        !eval_result.diagnostics.has_errors(),
        "eval produced unexpected errors: {:?}",
        eval_result.diagnostics
    );
}

#[test]
fn config_allowed_physical_metadata() {
    let result = eval_zen(vec![
        (
            "Module.zen".to_string(),
            r#"
                Capacitance = builtin.physical_value("F")

                capacitance = config(
                    "capacitance",
                    Capacitance,
                    allowed = ["100mF", "220mF"],
                    default = "100mF",
                )

                print("capacitance:", capacitance)
            "#
            .to_string(),
        ),
        (
            "top.zen".to_string(),
            r#"
                Mod = Module("Module.zen")

                Mod(name = "U1", capacitance = "0.1F")
            "#
            .to_string(),
        ),
    ]);

    assert!(
        result.is_success(),
        "expected eval success, got diagnostics: {:?}",
        result.diagnostics
    );

    let output = result.output.expect("expected eval output");
    let module_tree = output.module_tree();
    let child_module = module_tree
        .values()
        .find(|module| module.path().name() == "U1")
        .expect("expected instantiated child module");
    let param = child_module
        .signature()
        .iter()
        .find(|param| param.name == "capacitance")
        .expect("expected capacitance parameter in child signature");

    assert_eq!(
        param.allowed_values.as_ref().map(Vec::len),
        Some(2),
        "expected two allowed values in signature metadata"
    );
    let default_display = param
        .default_value
        .as_ref()
        .expect("expected normalized default value")
        .to_value()
        .to_repr();
    let allowed_display: Vec<String> = param
        .allowed_values
        .as_ref()
        .expect("expected normalized allowed values")
        .iter()
        .map(|value| value.to_value().to_repr())
        .collect();
    let actual_display = param
        .actual_value
        .as_ref()
        .expect("expected resolved config value")
        .to_value()
        .to_repr();

    assert!(
        allowed_display
            .iter()
            .any(|value| value == &default_display),
        "default should be one of the normalized allowed values: {:?}",
        allowed_display
    );
    assert!(
        allowed_display.iter().any(|value| value == &actual_display),
        "resolved value should match one of the normalized allowed values: {:?}",
        allowed_display
    );
}

snapshot_eval!(config_allowed_invalid_value, {
    "Module.zen" => r#"
        package = config(
            str,
            allowed = {"0402": 1, "0603": 2},
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(name = "U1", package = "0805")
    "#
});

snapshot_eval!(config_allowed_invalid_default, {
    "Module.zen" => r#"
        output_voltage = config(
            "output_voltage",
            Voltage,
            allowed = ["1.0V"],
            default = "0.9V",
        )
    "#,
    "top.zen" => r#"
        Mod = Module("Module.zen")

        Mod(name = "U1", output_voltage = "1.0V")
    "#
});

snapshot_eval!(config_allowed_unsupported_type, {
    "test.zen" => r#"
        Range = record(
            min = field(float),
            max = field(float),
        )

        value = config(
            "value",
            Range,
            allowed = [Range(min = 0.0, max = 1.0)],
        )
    "#
});
