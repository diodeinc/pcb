#![cfg(not(target_os = "windows"))]

use httpmock::prelude::*;
use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;
use serde_json::{Value, json};

const LED_MODULE_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

Resistor = Module("@stdlib/generics/Resistor.zen")
Led = Module("@stdlib/generics/Led.zen")

led_color = config(str, default = "red")
r_value = config(str, default = "330Ohm")
package = config(str, default = "0603")

VCC = io(Power)
GND = io(Ground)
CTRL = io(Gpio)

led_anode = Net("LED_ANODE")

Resistor(name = "R1", value = r_value, package = package, P1 = VCC, P2 = led_anode)
Led(name = "D1", color = led_color, package = package, A = led_anode, K = CTRL)
"#;

const TEST_BOARD_ZEN: &str = r#"
load("@stdlib/interfaces.zen", "Gpio")

LedModule = Module("../modules/LedModule.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")
Crystal = Module("@stdlib/generics/Crystal.zen")

vcc_3v3 = Power("VCC_3V3")
gnd = Ground("GND")
led_ctrl = Gpio("LED_CTRL")
osc_xi = Gpio("OSC_XI")
osc_xo = Gpio("OSC_XO")

Capacitor(name = "C1", value = "100nF", package = "0402", P1 = vcc_3v3, P2 = gnd)
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc_3v3, P2 = gnd)

LedModule(name = "LED1", led_color = "green", VCC = vcc_3v3, GND = gnd, CTRL = led_ctrl)
LedModule(name = "LED2", led_color = "red", VCC = vcc_3v3, GND = gnd, CTRL = Gpio(gnd))

Crystal(name = "X1", frequency = "16MHz", load_capacitance = "18pF", package = "5032_2Pin", XIN = osc_xi, XOUT = osc_xo)

Capacitor(name = "C3", value = "22pF", package = "0402", P1 = osc_xi, P2 = gnd)
Capacitor(name = "C4", value = "22pF", package = "0402", P1 = osc_xo, P2 = gnd)

Resistor(name = "R1", value = "10kOhm", package = "0603", P1 = vcc_3v3, P2 = led_ctrl)
"#;

const PARAMETRIC_GENERICS_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

FerriteBead = Module("@stdlib/generics/FerriteBead.zen")
Inductor = Module("@stdlib/generics/Inductor.zen")
Crystal = Module("@stdlib/generics/Crystal.zen")
Led = Module("@stdlib/generics/Led.zen")
PinHeader = Module("@stdlib/generics/PinHeader.zen")
Rectifier = Module("@stdlib/generics/Rectifier.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")
Tvs = Module("@stdlib/generics/Tvs.zen")
Zener = Module("@stdlib/generics/Zener.zen")

vcc = Power("VCC")
gnd = Ground("GND")
signal = Net("SIGNAL")
osc_in = Net("OSC_IN")
osc_out = Net("OSC_OUT")

Resistor(name = "R1", value = "10kOhm", package = "0603", power = "250mW", P1 = vcc, P2 = signal)
Inductor(name = "L1", value = "4.7uH", package = "0603", current = "1A", dcr = "120mOhm", P1 = vcc, P2 = signal)
FerriteBead(name = "FB1", value = "600Ohm", package = "0603", frequency = "100MHz", current = "500mA", dcr = "200mOhm", P1 = vcc, P2 = signal)
Crystal(name = "Y1", package = "3215_2Pin", frequency = "32.768kHz", load_capacitance = "12.5pF", esr = "70kOhm", XIN = osc_in, XOUT = osc_out)
Led(name = "D2", package = "0402", color = "red", forward_voltage = "2V", forward_current = "20mA", A = signal, K = gnd)
PinHeader(name = "J1", mount = "THT", pins = 2, rows = 1, pitch = "2.54mm", orientation = "Vertical", gender = "Male", P1 = vcc, P2 = gnd)
Rectifier(name = "D1", package = "SOD-123", technology = "Schottky", reverse_voltage = "40V", forward_current = "1A", forward_voltage = "500mV", A = signal, K = gnd)
Zener(name = "Z1", package = "SOD-123", zener_voltage = "5.1V", power = "500mW", A = signal, K = gnd)
Tvs(name = "TVS1", package = "DO-219AB", direction = "Unidirectional", reverse_standoff_voltage = "5V", reverse_clamping_voltage = "9V", peak_pulse_power = "400W", capacitance = "30pF", A = gnd, K = signal)
"#;

const SIMPLE_RESISTOR_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

Resistor = Module("@stdlib/generics/Resistor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Resistor(name = "R1", value = "1kOhm", package = "0603", P1 = vcc, P2 = gnd)
Resistor(name = "R2", value = "1kOhm", package = "0603", P1 = vcc, P2 = gnd)
Resistor(name = "R3", value = "4.7kOhm", package = "0402", P1 = vcc, P2 = gnd)
"#;

const CAPACITOR_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

Capacitor = Module("@stdlib/generics/Capacitor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

Capacitor(name = "C1", value = "100nF", package = "0402", voltage = "16V", dielectric = "X7R", P1 = vcc, P2 = gnd)
Capacitor(name = "C2", value = "10uF", package = "0805", voltage = "25V", dielectric = "X5R", P1 = vcc, P2 = gnd)
Capacitor(name = "C3", value = "1uF", package = "0603", P1 = vcc, P2 = gnd)
"#;

const SKIP_BOM_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

P1 = Net()
P2 = Net()

# Normal resistor - should appear in BOM
Component(
    name = "normal",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = Part(mpn = "RC0603FR-071KL", manufacturer = "Yageo"),
)

# Component with skip_bom kwarg - should NOT appear in BOM
Component(
    name = "skip_bom_kwarg",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    skip_bom = True,
)

# Component with skip_bom kwarg - should NOT appear in BOM
Component(
    name = "skip_bom_kwarg_2",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    skip_bom = True,
)

# Normal resistor - should appear in BOM
Component(
    name = "normal2",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = Part(mpn = "RC0603FR-0710KL", manufacturer = "Yageo"),
)
"#;

const DNP_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

P1 = Net()
P2 = Net()

# Normal component - should appear in BOM with dnp omitted (false)
Component(
    name = "normal",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = Part(mpn = "RC0603FR-071KL", manufacturer = "Yageo"),
)

# DNP component (via dnp kwarg) - should appear in BOM with dnp=true
Component(
    name = "dnp_kwarg_1",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = Part(mpn = "RC0603FR-0710KL", manufacturer = "Yageo"),
    dnp = True,
)

# DNP component (via dnp kwarg) - should appear in BOM with dnp=true
Component(
    name = "dnp_kwarg_2",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = Part(mpn = "RC0603FR-074K7L", manufacturer = "Yageo"),
    dnp = True,
)

# Component with both DNP and skip_bom - should NOT appear in BOM (skip_bom wins)
Component(
    name = "dnp_and_skip_bom",
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0603_1608Metric.kicad_mod"),
    pin_defs = {"1": "1", "2": "2"},
    pins = {"1": P1, "2": P2},
    part = Part(mpn = "RC0603FR-07100KL", manufacturer = "Yageo"),
    dnp = True,
    skip_bom = True,
)
"#;

const MODULE_DNP_BOARD_ZEN: &str = r#"
# ```pcb
# [workspace]
# pcb-version = "0.4"
# ```

Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")

vcc = Power("VCC")
gnd = Ground("GND")

# Normal module - components should NOT be DNP
Resistor(name = "R1", value = "1kOhm", package = "0603", P1 = vcc, P2 = gnd)

# Module with dnp=True - properties must not erase DNP in either argument order
Resistor(name = "R2_DNP", value = "10kOhm", package = "0603", P1 = vcc, P2 = gnd, dnp = True, properties = {})
Capacitor(name = "C1_DNP", value = "100nF", package = "0402", P1 = vcc, P2 = gnd, properties = {}, dnp = True)

# Normal module again - should NOT be DNP
Capacitor(name = "C2", value = "10uF", package = "0805", P1 = vcc, P2 = gnd)
"#;

const WORKSPACE_TOML: &str = r#"
[workspace]
pcb-version = "0.4"
"#;

#[test]
fn test_bom_json_format() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .sync()
        .snapshot_run(
            "pcbc",
            ["bom", "boards/TestBoard.zen", "--offline", "-f", "json"],
        );
    assert_snapshot!("bom_json", output);
}

#[test]
fn test_parametric_generics_json_format() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write(
            "boards/ParametricGenerics.zen",
            PARAMETRIC_GENERICS_BOARD_ZEN,
        )
        .sync()
        .snapshot_run(
            "pcbc",
            [
                "bom",
                "boards/ParametricGenerics.zen",
                "--offline",
                "-f",
                "json",
            ],
        );
    assert_snapshot!("bom_parametric_generics_json", output);
}

#[test]
fn test_bom_table_format() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("modules/LedModule.zen", LED_MODULE_ZEN)
        .write("boards/TestBoard.zen", TEST_BOARD_ZEN)
        .sync()
        .snapshot_run(
            "pcbc",
            ["bom", "boards/TestBoard.zen", "--offline", "-f", "table"],
        );
    assert_snapshot!("bom_table", output);
}

#[test]
fn bom_fails_when_availability_cannot_be_fetched() {
    // The sandbox routes HTTP through a dead proxy, so the BOM match request fails.
    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/SimpleResistors.zen", SIMPLE_RESISTOR_BOARD_ZEN);
    for format in ["json", "table"] {
        let output = sandbox
            .run("pcbc", ["bom", "boards/SimpleResistors.zen", "-f", format])
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .expect("BOM command should run");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{format}: expected failure");
        assert!(output.stdout.is_empty(), "{format}: printed a BOM anyway");
        assert!(
            stderr.contains("Failed to fetch availability data") && stderr.contains("--offline"),
            "{format}: {stderr}"
        );
    }
}

#[test]
fn bom_uses_workspace_inventory_when_signed_in_and_still_works_anonymously() {
    for (workspace_config, signed_in, disable_auth) in [
        ("name = \"acme\"", true, false),
        (
            "repository = \"https://github.com/acme/boards.git\"",
            true,
            false,
        ),
        ("name = \"acme\"", false, false),
        ("name = \"acme\"", true, true),
    ] {
        let server = MockServer::start();
        let authenticated = signed_in && !disable_auth;
        let (offer_id, stock) = if authenticated {
            ("reserve", 176)
        } else {
            ("public", 5000)
        };
        let bom_match = server.mock(|when, then| {
            let when = when
                .method(POST)
                .path("/api/boms/match")
                .json_body_includes(r#"{"workspace":"acme","regions":["US","GLOBAL"]}"#);
            if authenticated {
                when.header("authorization", "Bearer workspace-member-token");
            } else {
                when.header_missing("authorization");
            }
            then.status(200).json_body(json!({
                "lines": (["R1.R", "R2.R", "R3.R"].map(|path| json!({
                    "designEntry": {"path": path},
                    "match": "MATCH_COMPATIBLE",
                    "ranked": {"US": [{"offerId": offer_id, "stockClass": "PLENTY"}]}
                }))),
                "offers": {(offer_id): {
                    "id": offer_id,
                    "geography": "US",
                    "sellerName": if authenticated { "Diode (Reserve)" } else { "DigiKey" },
                    "marketStock": stock,
                    "priceBreaks": [{"quantity": 1, "unitPrice": {"amountMicros": 250_000, "currency": "USD"}}]
                }}
            }));
        });
        let mut sandbox = Sandbox::new();
        sandbox
            .env("DIODE_API_URL", server.base_url())
            .env("NO_PROXY", "127.0.0.1,localhost")
            .write(
                "pcb.toml",
                format!("{WORKSPACE_TOML}\n{workspace_config}\n"),
            )
            .write("boards/SimpleResistors.zen", SIMPLE_RESISTOR_BOARD_ZEN);
        if signed_in {
            let auth_file = sandbox
                .home
                .join(format!(".pcb/auth/http_127_0_0_1_{}.toml", server.port()));
            sandbox.write(
                auth_file,
                "access_token = \"workspace-member-token\"\nrefresh_token = \"refresh\"\nexpires_at = 4102444800\n",
            );
        }
        if disable_auth {
            sandbox.env("DIODE_API_AUTH", "none");
        }
        let output = sandbox
            .run("pcbc", ["bom", "boards/SimpleResistors.zen", "-f", "json"])
            .stdout_capture()
            .stderr_capture()
            .unchecked()
            .run()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let groups: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            groups
                .iter()
                .map(|group| group["quantity"].as_u64().unwrap())
                .sum::<u64>(),
            3
        );
        for group in &groups {
            let availability = &group["availability"];
            assert_eq!(availability["selected_offer_id"], offer_id);
            assert_eq!(availability["us"]["stock"], stock);
            assert_eq!(availability["us"]["price"], 0.25);
        }
        bom_match.assert_calls(1);
    }
}

#[test]
fn test_bom_simple_resistors() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/SimpleResistors.zen", SIMPLE_RESISTOR_BOARD_ZEN)
        .sync()
        .snapshot_run(
            "pcbc",
            [
                "bom",
                "boards/SimpleResistors.zen",
                "--offline",
                "-f",
                "json",
            ],
        );
    assert_snapshot!("bom_simple_resistors_json", output);
}

#[test]
fn test_bom_capacitors_with_dielectric() {
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/Capacitors.zen", CAPACITOR_BOARD_ZEN)
        .sync()
        .snapshot_run(
            "pcbc",
            ["bom", "boards/Capacitors.zen", "--offline", "-f", "json"],
        );
    assert_snapshot!("bom_capacitors_json", output);
}

#[test]
fn test_bom_skip_bom_filtering() {
    // Test that components with skip_bom are excluded from BOM output
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/SkipBom.zen", SKIP_BOM_BOARD_ZEN)
        .sync()
        .snapshot_run(
            "pcbc",
            ["bom", "boards/SkipBom.zen", "--offline", "-f", "json"],
        );
    assert_snapshot!("bom_skip_bom_json", output);
}

#[test]
fn test_bom_dnp_components() {
    // Test that DNP components appear in BOM (dnp is for assembly, not procurement)
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/DnpBoard.zen", DNP_BOARD_ZEN)
        .sync()
        .snapshot_run(
            "pcbc",
            ["bom", "boards/DnpBoard.zen", "--offline", "-f", "json"],
        );
    assert_snapshot!("bom_dnp_json", output);
}

#[test]
fn test_bom_module_dnp_propagation() {
    // Test that module-level dnp=True propagates to all child components
    let output = Sandbox::new()
        .write("pcb.toml", WORKSPACE_TOML)
        .write("boards/ModuleDnp.zen", MODULE_DNP_BOARD_ZEN)
        .sync()
        .snapshot_run(
            "pcbc",
            ["bom", "boards/ModuleDnp.zen", "--offline", "-f", "json"],
        );
    assert_snapshot!("bom_module_dnp_json", output);
}

#[test]
fn bom_json_uses_symbol_description_for_untyped_component() {
    const SYMBOL: &str = r#"(kicad_symbol_lib (version 20231120) (generator kicad_symbol_editor)
  (symbol "TCXO"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "TCXO" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "Datasheet" "" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "Description" "Temperature-compensated crystal oscillator" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "Manufacturer_Name" "Acme" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (property "Manufacturer_Part_Number" "ACME-TCXO-32" (at 0 0 0) (effects (font (size 1.27 1.27)) hide))
    (symbol "TCXO_1_1"
      (pin input line (at -2.54 0 0) (length 2.54)
        (name "EN" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))))))"#;
    const BOARD: &str = r#"enable = Net("ENABLE")
Component(
    name = "oscillator",
    symbol = Symbol(library = "TCXO.kicad_sym"),
    footprint = "Package_DirectFET:DirectFET_L4",
    properties = {"value": "32MHz"},
    pins = {"EN": enable},
)
"#;

    let mut sandbox = Sandbox::new();
    sandbox
        .write("pcb.toml", "[workspace]\n")
        .write("TCXO.kicad_sym", SYMBOL)
        .write("TCXO.zen", BOARD);

    let output = sandbox
        .run("pcbc", ["bom", "TCXO.zen", "--offline", "-f", "json"])
        .stdout_capture()
        .stderr_capture()
        .unchecked()
        .run()
        .expect("BOM command should run");
    assert!(
        output.status.success(),
        "BOM command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let bom: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("BOM output should be valid JSON");
    let entry = &bom[0]["members"][0];
    assert_eq!(entry["value"], "32MHz");
    assert_eq!(
        entry["description"],
        "Temperature-compensated crystal oscillator"
    );
    assert_eq!(entry["manufacturer"], "Acme");
    assert_eq!(entry["mpn"], "ACME-TCXO-32");
}
