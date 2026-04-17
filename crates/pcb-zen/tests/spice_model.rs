mod common;
use common::TestProject;

use pcb_sim::gen_sim;

#[macro_export]
macro_rules! sim_snapshot {
    ($env:expr, $entry:expr $(,)?) => {{
        let top_path = $env.root().join($entry);

        let file_provider = pcb_zen_core::DefaultFileProvider::new();
        let mut workspace_info =
            pcb_zen::get_workspace_info(&file_provider, &top_path, true).expect("get workspace info");
        let res = pcb_zen::resolve_dependencies(&mut workspace_info, false, false)
            .expect("dependency resolution");

        let mut buf = Vec::new();
        let schematic = pcb_zen::run(&top_path, res, Default::default())
            .output_result()
            .expect("failed to compile schematic for simulation");
        gen_sim(&schematic, &mut buf)
            .expect("failed to generate .cir contents");
        let result = String::from_utf8(buf).unwrap();

        let root_path = $env.root().to_string_lossy();

        // Get the cache directory path for filtering
        let cache_dir_path = pcb_zen::cache_index::cache_base().to_string_lossy().into_owned();

        // Create regex patterns as owned values
        let temp_dir_pattern = ::regex::escape(&format!("{}{}", root_path, std::path::MAIN_SEPARATOR));
        let cache_dir_pattern = if !cache_dir_path.is_empty() {
            Some(::regex::escape(&format!("{}{}", cache_dir_path, std::path::MAIN_SEPARATOR)))
        } else {
            None
        };

        let mut filters = vec![
            (temp_dir_pattern.as_ref(), "[TEMP_DIR]"),
        ];

        // Add cache directory filter if it exists
        if let Some(cache_pattern) = cache_dir_pattern.as_ref() {
            filters.push((cache_pattern.as_ref(), "[CACHE_DIR]"));
        }

        insta::with_settings!({
            filters => filters,
        }, {
            insta::assert_snapshot!(result);
        });
    }};
}

#[test]
fn snapshot_sim_divider() {
    let env = TestProject::new();

    env.add_file(
        "r.lib",
        r#"
.SUBCKT my_resistor p n PARAMS: RVAL={0}
R1 p n {RVAL}
.ENDS my_resistor
"#,
    );

    env.add_file(
        "myresistor.zen",
        r#"
load("@stdlib/config.zen", "config_properties")
load("@stdlib/units.zen", "Resistance", "Voltage")
load("@stdlib/utils.zen", "format_value")

# -----------------------------------------------------------------------------
# Component types
# -----------------------------------------------------------------------------

Package = enum("0201", "0402", "0603", "0805", "1206", "1210", "2010", "2512")

# -----------------------------------------------------------------------------
# Component parameters
# -----------------------------------------------------------------------------

# Required
package = config(Package, default = Package("0603"))
value = config(Resistance)

# Optional
voltage = config(Voltage, optional = True)

# Properties - combined and normalized
properties = config_properties({
    "value": format_value(value, voltage),
    "package": package,
    "resistance": value,
    "voltage": voltage,
})

# -----------------------------------------------------------------------------
# IO ports
# -----------------------------------------------------------------------------

P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    symbol = Symbol(library = "@kicad-symbols/Device.kicad_sym", name="R"),
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0201_0603Metric.kicad_mod"),
    prefix = "R",
    spice_model = SpiceModel('./r.lib', 'my_resistor',
        nets=[P1, P2],
        args={"RVAL": str(value.value)}),
    pins = {
        "1": P1,
        "2": P2,
    },
    properties = properties,
)
"#,
    );

    env.add_file(
        "divider.zen",
        r#"
load("@stdlib/interfaces.zen", "Analog")
Resistor = Module("myresistor.zen")

# Configuration parameters
r1_value = config(str, default="10kohms", optional=True)
r2_value = config(str, default="20kohms", optional=True)

# IO ports
vin = io(Power)
vout = io(Analog)
gnd = io(Ground)

# Create the voltage divider
Resistor(name="R1", value=r1_value, package="0603", P1=vin, P2=vout)
Resistor(name="R2", value=r2_value, package="0603", P1=vout, P2=gnd)
"#,
    );

    sim_snapshot!(env, "divider.zen");
}

#[test]
fn snapshot_sim_setup_inline() {
    let env = TestProject::new();

    env.add_file(
        "r.lib",
        r#"
.SUBCKT my_resistor p n PARAMS: RVAL={0}
R1 p n {RVAL}
.ENDS my_resistor
"#,
    );

    env.add_file(
        "myresistor.zen",
        r#"
load("@stdlib/config.zen", "config_properties")
load("@stdlib/units.zen", "Resistance", "Voltage")
load("@stdlib/utils.zen", "format_value")

Package = enum("0201", "0402", "0603", "0805", "1206", "1210", "2010", "2512")

package = config(Package, default = Package("0603"))
value = config(Resistance)
voltage = config(Voltage, optional = True)

properties = config_properties({
    "value": format_value(value, voltage),
    "package": package,
    "resistance": value,
    "voltage": voltage,
})

P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    symbol = Symbol(library = "@kicad-symbols/Device.kicad_sym", name="R"),
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0201_0603Metric.kicad_mod"),
    prefix = "R",
    spice_model = SpiceModel('./r.lib', 'my_resistor',
        nets=[P1, P2],
        args={"RVAL": str(value.value)}),
    pins = {
        "1": P1,
        "2": P2,
    },
    properties = properties,
)
"#,
    );

    env.add_file(
        "divider.zen",
        r#"
load("@stdlib/interfaces.zen", "Analog")
Resistor = Module("myresistor.zen")

r1_value = config(str, default="10kohms", optional=True)
r2_value = config(str, default="20kohms", optional=True)

vin = io(Power)
vout = io(Analog)
gnd = io(Ground)

Resistor(name="R1", value=r1_value, package="0603", P1=vin, P2=vout)
Resistor(name="R2", value=r2_value, package="0603", P1=vout, P2=gnd)

builtin.set_sim_setup(content="V1 vin gnd DC 5\n.tran 1u 10m\n.end\n")
"#,
    );

    sim_snapshot!(env, "divider.zen");
}

#[test]
fn snapshot_sim_divider_from_symbol_metadata() {
    let env = TestProject::new();

    env.add_file(
        "r.lib",
        r#"
.SUBCKT my_resistor p n PARAMS: RVAL={0}
R1 p n {RVAL}
.ENDS my_resistor
"#,
    );

    env.add_file(
        "myresistor.kicad_sym",
        r#"(kicad_symbol_lib (version 20211014) (generator kicad_symbol_editor)
  (symbol "MyResistor" (pin_names (offset 1.016)) (in_bom yes) (on_board yes)
    (property "Reference" "R" (id 0) (at 0 0 0))
    (property "Sim.Library" "r.lib" (id 1) (at 0 0 0))
    (property "Sim.Name" "my_resistor" (id 2) (at 0 0 0))
    (property "Sim.Device" "SUBCKT" (id 3) (at 0 0 0))
    (property "Sim.Pins" "1=p 2=n" (id 4) (at 0 0 0))
    (property "Sim.Params" "RVAL=10000.0" (id 5) (at 0 0 0))
    (symbol "MyResistor_0_1"
      (rectangle (start -10.16 10.16) (end 10.16 -10.16))
    )
    (symbol "MyResistor_1_1"
      (pin passive line (at -12.7 2.54 0) (length 2.54)
        (name "P1" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27))))
      )
      (pin passive line (at -12.7 -2.54 0) (length 2.54)
        (name "P2" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27))))
      )
    )
  )
)"#,
    );

    env.add_file(
        "myresistor.zen",
        r#"
load("@stdlib/config.zen", "config_properties")
load("@stdlib/units.zen", "Resistance", "Voltage")
load("@stdlib/utils.zen", "format_value")

Package = enum("0201", "0402", "0603", "0805", "1206", "1210", "2010", "2512")

package = config("package", Package, default = Package("0603"))
value = config("value", Resistance)
voltage = config("voltage", Voltage, optional = True)

properties = config_properties({
    "value": format_value(value, voltage),
    "package": package,
    "resistance": value,
    "voltage": voltage,
})

P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "R",
    symbol = Symbol(library = "myresistor.kicad_sym"),
    footprint = "TEST:0402",
    prefix = "R",
    pins = {
        "P1": P1,
        "P2": P2,
    },
    properties = properties,
)
"#,
    );

    env.add_file(
        "divider.zen",
        r#"
Resistor = Module("myresistor.zen")

vin = io("vin", Power)
vout = io("vout", Net)
gnd = io("gnd", Ground)

Resistor(name="R1", value="10kohms", package="0603", P1=vin, P2=vout)
Resistor(name="R2", value="10kohms", package="0603", P1=vout, P2=gnd)
"#,
    );

    sim_snapshot!(env, "divider.zen");
}

#[test]
fn snapshot_sim_setup_file() {
    let env = TestProject::new();

    env.add_file(
        "r.lib",
        r#"
.SUBCKT my_resistor p n PARAMS: RVAL={0}
R1 p n {RVAL}
.ENDS my_resistor
"#,
    );

    env.add_file(
        "myresistor.zen",
        r#"
load("@stdlib/config.zen", "config_properties")
load("@stdlib/units.zen", "Resistance", "Voltage")
load("@stdlib/utils.zen", "format_value")

Package = enum("0201", "0402", "0603", "0805", "1206", "1210", "2010", "2512")

package = config(Package, default = Package("0603"))
value = config(Resistance)
voltage = config(Voltage, optional = True)

properties = config_properties({
    "value": format_value(value, voltage),
    "package": package,
    "resistance": value,
    "voltage": voltage,
})

P1 = io(Net)
P2 = io(Net)

Component(
    name = "R",
    symbol = Symbol(library = "@kicad-symbols/Device.kicad_sym", name="R"),
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0201_0603Metric.kicad_mod"),
    prefix = "R",
    spice_model = SpiceModel('./r.lib', 'my_resistor',
        nets=[P1, P2],
        args={"RVAL": str(value.value)}),
    pins = {
        "1": P1,
        "2": P2,
    },
    properties = properties,
)
"#,
    );

    env.add_file("setup.spice", "V1 vin gnd DC 5\n.tran 1u 10m\n.end\n");

    env.add_file(
        "divider.zen",
        r#"
load("@stdlib/interfaces.zen", "Analog")
Resistor = Module("myresistor.zen")

r1_value = config(str, default="10kohms", optional=True)
r2_value = config(str, default="20kohms", optional=True)

vin = io(Power)
vout = io(Analog)
gnd = io(Ground)

Resistor(name="R1", value=r1_value, package="0603", P1=vin, P2=vout)
Resistor(name="R2", value=r2_value, package="0603", P1=vout, P2=gnd)

builtin.set_sim_setup(file="setup.spice")
"#,
    );

    sim_snapshot!(env, "divider.zen");
}

#[test]
fn sim_setup_duplicate_error() {
    let env = TestProject::new();

    env.add_file(
        "test.zen",
        r#"
builtin.set_sim_setup(content="V1 vin gnd DC 5")
builtin.set_sim_setup(content=".tran 1u 10m")
"#,
    );

    let top_path = env.root().join("test.zen");
    let file_provider = pcb_zen_core::DefaultFileProvider::new();
    let mut workspace_info =
        pcb_zen::get_workspace_info(&file_provider, &top_path, true).expect("get workspace info");
    let res = pcb_zen::resolve_dependencies(&mut workspace_info, false, false)
        .expect("dependency resolution");

    let result = pcb_zen::eval(&top_path, res, Default::default());
    assert!(
        result.output.is_none(),
        "expected evaluation to fail due to duplicate set_sim_setup"
    );
    let diag_text = format!("{:?}", result.diagnostics);
    assert!(
        diag_text.contains("Sim setup already set"),
        "expected 'Sim setup already set' in diagnostics, got: {}",
        diag_text
    );
}
