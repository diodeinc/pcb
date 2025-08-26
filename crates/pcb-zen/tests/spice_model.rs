mod common;
use common::TestProject;

use pcb_sim::gen_sim;

#[macro_export]
macro_rules! sim_snapshot {
    ($env:expr, $entry:expr $(,)?) => {{
        let top_path = $env.root().join($entry);

        let mut buf = Vec::new();
        let schematic = pcb_zen::run(&top_path, false)
            .output_result()
            .expect("failed to compile schematic for simulation");
        gen_sim(&schematic, &mut buf)
            .expect("failed to generate .cir contents");
        let result = String::from_utf8(buf).unwrap();

        let root_path = $env.root().to_string_lossy();

        // Get the cache directory path for filtering
        let cache_dir_path = pcb_zen::load::cache_dir()
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

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
load("@stdlib/generics/SolderJumper.zen", "pin_defs")
load("@stdlib/config.zen", "config_properties", "config_unit")
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
package = config("package", Package, default = Package("0603"))
value = config_unit("value", Resistance)

# Optional
voltage = config_unit("voltage", Voltage, optional = True)

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

P1 = io("P1", Net)
P2 = io("P2", Net)

Component(
    name = "R",
    symbol = Symbol(library = "@kicad-symbols/Device.kicad_sym", name="R"),
    footprint = File("@kicad-footprints/Resistor_SMD.pretty/R_0201_0603Metric.kicad_mod"),
    prefix = "R",
    spice_model = SpiceModel('./r.lib', 'my_resistor',
        nets=[P1, P2],
        args={"RVAL": str(value.value)}),
    pin_defs = {
        "P1": "1",
        "P2": "2",
    },
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
load("@stdlib/interfaces.zen", "Power", "Ground", "Analog")
Resistor = Module("myresistor.zen")

# Configuration parameters
r1_value = config("r1_value", str, default="10kohms", optional=True)
r2_value = config("r2_value", str, default="20kohms", optional=True)

# IO ports
vin = io("vin", Power)
vout = io("vout", Analog)
gnd = io("gnd", Ground)

# Create the voltage divider
Resistor(name="R1", value=r1_value, package="0603", P1=vin.NET, P2=vout.NET)
Resistor(name="R2", value=r2_value, package="0603", P1=vout.NET, P2=gnd.NET)
"#,
    );

    sim_snapshot!(env, "divider.zen");
}
