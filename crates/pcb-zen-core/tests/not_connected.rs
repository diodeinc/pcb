use pcb_zen_core::{DiagnosticsPass, SortPass};

mod common;

fn eval_to_schematic(
    files: std::collections::HashMap<String, String>,
    main: &str,
) -> pcb_zen_core::WithDiagnostics<pcb_sch::Schematic> {
    let mut all_files = common::stdlib_test_files();
    all_files.extend(files);
    let result = common::eval_zen_raw(all_files, main);
    assert!(result.is_success(), "eval failed: {:?}", result.diagnostics);
    let eval_output = result.output.expect("expected EvalOutput on success");
    eval_output.to_schematic_with_diagnostics()
}

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_warns_on_multiple_ports() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
NotConnected = builtin.not_connected
nc = NotConnected("NC_PIN")

Component(
    name = "R1",
    prefix = "R",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)

Component(
    name = "R2",
    prefix = "R",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)
"#
        .to_string(),
    );

    let eval_result = common::eval_zen(vec![(
        "test.zen".to_string(),
        files.get("test.zen").expect("test file exists").clone(),
    )]);
    assert!(
        eval_result.diagnostics.warnings().iter().any(|w| {
            w.body == "NotConnected does not support names; name ignored" && w.span.is_some()
        }),
        "expected spanned NotConnected name warning, got: {:?}",
        eval_result.diagnostics
    );

    let mut result = eval_to_schematic(files, "test.zen");
    SortPass.apply(&mut result.diagnostics);
    let warnings = result.diagnostics.warnings();
    assert!(
        warnings
            .iter()
            .any(|w| w.body.contains("NotConnected net connects to 2 ports")),
        "expected multi-port NotConnected warning, got: {:?}",
        warnings
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.body.contains("R1.P2") && w.body.contains("R2.P2")),
        "expected warning to mention ports, got: {:?}",
        warnings
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_does_not_warn_on_single_port_multiple_pads() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
NotConnected = builtin.not_connected
nc = NotConnected()

Component(
    name = "U1",
    prefix = "U",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    symbol = Symbol(
        definition = [
            ("GND", ["5", "17"]),
        ]
    ),
    pins = {"GND": nc},
)
"#
        .to_string(),
    );

    let mut result = eval_to_schematic(files, "test.zen");
    SortPass.apply(&mut result.diagnostics);
    let warnings = result.diagnostics.warnings();
    assert!(
        warnings
            .iter()
            .all(|w| !w.body.contains("NotConnected net connects to")),
        "did not expect multi-port NotConnected warning, got: {:?}",
        warnings
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn omitted_no_connect_pin_converts_to_not_connected_net() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "nc_pin.kicad_sym".to_string(),
        r#"(kicad_symbol_lib
  (version 20211014)
  (generator "test")
  (symbol "NcPin"
    (property "Reference" "U")
    (symbol "NcPin_0_1"
      (pin no_connect line
        (at 0 0 0)
        (length 2.54)
        (name "NC")
        (number "1")
      )
    )
  )
)"#
        .to_string(),
    );
    files.insert(
        "test.zen".to_string(),
        r#"
symbol = Symbol(library = "nc_pin.kicad_sym")

Component(
    name = "U1",
    prefix = "U",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    symbol = symbol,
    skip_bom = True,
    pins = {},
)
"#
        .to_string(),
    );

    let result = eval_to_schematic(files, "test.zen");
    assert!(
        result.is_success(),
        "expected schematic output, got diagnostics: {:?}",
        result.diagnostics
    );
    let schematic = result.output.expect("expected schematic output");
    assert!(!schematic.nets.contains_key(""));
    assert!(schematic.nets.values().any(|net| {
        net.kind == "NotConnected"
            && net.ports.iter().any(|port| {
                port.instance_path
                    .ends_with(&["U1".to_string(), "NC".to_string()])
            })
    }));
}

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_callable_is_not_a_net_type() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
NotConnected = builtin.not_connected

NC = io(NotConnected)
"#
        .to_string(),
    )]);

    assert!(!result.is_success(), "expected eval failure");
    let rendered = result
        .diagnostics
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("builtin.io() requires a Net or interface type, got function"),
        "expected NotConnected-as-type rejection, got: {rendered}"
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_cannot_be_defined_as_net_type() {
    let result = common::eval_zen(vec![(
        "test.zen".to_string(),
        r#"
NotConnected = builtin.net_type("NotConnected")
"#
        .to_string(),
    )]);

    assert!(!result.is_success(), "expected eval failure");
    let rendered = result
        .diagnostics
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        rendered.contains("NotConnected is an open-net constructor, not a net type"),
        "expected NotConnected net type rejection, got: {rendered}"
    );
}

#[test]
#[cfg(not(target_os = "windows"))]
fn open_not_connected_satisfies_typed_io_without_changing_kind() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "interfaces.zen".to_string(),
        r#"
Gpio = builtin.net_type("Gpio")
"#
        .to_string(),
    );
    files.insert(
        "child.zen".to_string(),
        r#"
load("interfaces.zen", "Gpio")
io = builtin.io

expected_gpio_type = config(str, default="Gpio")
GPIO = io(Gpio)
check(GPIO.type == expected_gpio_type, "open IO should keep its original net type")

Component(
    name = "R1",
    prefix = "R",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    skip_bom = True,
    pin_defs = {"P1": "1"},
    pins = {"P1": GPIO},
)
"#
        .to_string(),
    );
    files.insert(
        "test.zen".to_string(),
        r#"
Child = Module("child.zen")
NotConnected = builtin.not_connected

Child(name = "U1", GPIO = NotConnected(), expected_gpio_type = "Net")
"#
        .to_string(),
    );

    let result = eval_to_schematic(files, "test.zen");
    assert!(
        result.is_success(),
        "expected schematic output, got diagnostics: {:?}",
        result.diagnostics
    );
    let schematic = result.output.expect("expected schematic output");
    let net = schematic
        .nets
        .values()
        .find(|net| {
            net.ports.iter().any(|port| {
                port.instance_path.ends_with(&[
                    "U1".to_string(),
                    "R1".to_string(),
                    "P1".to_string(),
                ])
            })
        })
        .expect("expected net connected to child component pin");
    assert_eq!(net.kind, "NotConnected");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn default_not_connected_io_remains_open() {
    let mut files = std::collections::HashMap::new();
    files.insert(
        "test.zen".to_string(),
        r#"
Net = builtin.net_type("Net")
Power = builtin.net_type("Power")
NotConnected = builtin.not_connected
io = builtin.io

MH = io(Power, optional=True, default=NotConnected())
check(MH.type == "Net", "open default should keep its original net type")

Component(
    name = "J1",
    prefix = "J",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    skip_bom = True,
    pin_defs = {"MH": "1"},
    pins = {"MH": MH},
)
"#
        .to_string(),
    );

    let result = eval_to_schematic(files, "test.zen");
    assert!(
        result.is_success(),
        "expected schematic output, got diagnostics: {:?}",
        result.diagnostics
    );
    let schematic = result.output.expect("expected schematic output");
    let net = schematic
        .nets
        .values()
        .find(|net| {
            net.ports.iter().any(|port| {
                port.instance_path
                    .ends_with(&["J1".to_string(), "MH".to_string()])
            })
        })
        .expect("expected net connected to defaulted IO pin");
    assert_eq!(net.kind, "NotConnected");
}

#[test]
#[cfg(not(target_os = "windows"))]
fn net_wrapped_not_connected_io_is_regular_net() {
    for _ in 0..64 {
        let mut files = std::collections::HashMap::new();
        files.insert(
            "test.zen".to_string(),
            r#"
NotConnected = builtin.not_connected
Net = builtin.net_type("Net")
io = builtin.io

PGFB = io(Net(NotConnected()), optional=True)
VIN = Net("VIN")

Component(
    name = "IC1",
    prefix = "IC",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"PGFB": "1"},
    pins = {"PGFB": PGFB},
)

Component(
    name = "R_PGFB",
    prefix = "R",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P1": "1", "P2": "2"},
    pins = {"P1": PGFB, "P2": VIN},
)
"#
            .to_string(),
        );

        let mut result = eval_to_schematic(files, "test.zen");
        SortPass.apply(&mut result.diagnostics);
        let warnings = result.diagnostics.warnings();
        assert!(
            warnings
                .iter()
                .all(|w| !w.body.contains("NotConnected net connects to")),
            "did not expect multi-port NotConnected warning, got: {:?}",
            warnings
        );

        let schematic = result.output.unwrap_or_else(|| {
            panic!(
                "expected schematic output, got diagnostics: {:?}",
                result.diagnostics
            )
        });
        let pgfb = schematic.nets.get("PGFB").unwrap_or_else(|| {
            panic!(
                "expected PGFB net, got: {:?}",
                schematic
                    .nets
                    .iter()
                    .map(|(name, net)| (name.clone(), net.kind.clone()))
                    .collect::<Vec<_>>()
            )
        });
        assert_eq!(pgfb.kind, "Net");
    }
}

#[test]
#[cfg(not(target_os = "windows"))]
fn not_connected_auto_names_are_stable_by_port() {
    // Two programs that only differ in where unrelated nets are created.
    // The NotConnected net connected to R1.P2 should get a stable, port-derived name.
    let a = r#"
NotConnected = builtin.not_connected

_dummy1 = NotConnected()
_dummy2 = NotConnected()

nc = NotConnected()

Component(
    name = "R1",
    prefix = "R",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)
"#;

    let b = r#"
NotConnected = builtin.not_connected

nc = NotConnected()

Component(
    name = "R1",
    prefix = "R",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0402_1005Metric.kicad_mod"),
    pin_defs = {"P2": "2"},
    pins = {"P2": nc},
)

_dummy1 = NotConnected()
_dummy2 = NotConnected()
"#;

    let mut files_a = std::collections::HashMap::new();
    files_a.insert("test.zen".to_string(), a.to_string());
    let res_a = eval_to_schematic(files_a, "test.zen");
    let sch_a = res_a.output.expect("expected schematic output");

    let mut files_b = std::collections::HashMap::new();
    files_b.insert("test.zen".to_string(), b.to_string());
    let res_b = eval_to_schematic(files_b, "test.zen");
    let sch_b = res_b.output.expect("expected schematic output");

    fn find_net_name(schematic: &pcb_sch::Schematic) -> String {
        let needle: [String; 2] = ["R1".to_string(), "P2".to_string()];
        schematic
            .nets
            .iter()
            .find_map(|(name, net)| {
                if net.kind != "NotConnected" {
                    return None;
                }
                let has_port = net
                    .ports
                    .iter()
                    .any(|p| p.instance_path.as_slice().ends_with(&needle));
                has_port.then(|| name.clone())
            })
            .unwrap_or_else(|| {
                panic!(
                    "failed to find NotConnected net for port R1.P2 (nets: {:?})",
                    schematic
                        .nets
                        .iter()
                        .map(|(n, net)| (n.clone(), net.kind.clone()))
                        .collect::<Vec<_>>()
                )
            })
    }

    let name_a = find_net_name(&sch_a);
    let name_b = find_net_name(&sch_b);

    assert_eq!(name_a, "NC_R1_P2");
    assert_eq!(name_b, "NC_R1_P2");
    assert_eq!(
        sch_a
            .nets
            .values()
            .filter(|net| net.kind == "NotConnected")
            .count(),
        1
    );
    assert!(!sch_a.nets.contains_key(""));
}
