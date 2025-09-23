use pcb_test_utils::assert_snapshot;
use pcb_test_utils::sandbox::Sandbox;

const SIMPLE_METADATA_ZEN: &str = r#"
load("@stdlib:v0.2.10/units.zen", "Voltage", "Current", "Resistance")

# Test basic metadata container operations
voltage_meta = metadata(Voltage)
current_meta = metadata(Current)
resistance_meta = metadata(Resistance)

# Test pushing values
voltage_meta.push(Voltage("5V 5%"))
voltage_meta.push(Voltage("3.3V 1%"))
current_meta.push(Current("100mA 5%"))
resistance_meta.push(Resistance("1kOhm 5%"))

# Test get() - returns most recent
print("Latest voltage:", voltage_meta.get())
print("Latest current:", current_meta.get())
print("Latest resistance:", resistance_meta.get())

# Test list() - returns all in chronological order
print("All voltages:", voltage_meta.list())
print("All currents:", current_meta.list())
print("All resistances:", resistance_meta.list())

# Test empty container
empty_meta = metadata(Voltage)
print("Empty get:", empty_meta.get())
print("Empty list:", empty_meta.list())
"#;

const POWER_MODULE_ZEN: &str = r#"
load("@stdlib:v0.2.10/units.zen", "Voltage", "Current")

PowerSpec = interface(
    voltage_meta = metadata(Voltage),
    current_meta = metadata(Current),
)

voltage_rating = config("voltage_rating", str, default = "5V")
current_rating = config("current_rating", str, default = "1A")

SPEC = io("SPEC", PowerSpec)

# Store the configured ratings
SPEC.voltage_meta.push(Voltage(voltage_rating))
SPEC.current_meta.push(Current(current_rating))

print("Power module configured with:", SPEC.voltage_meta.get(), "at", SPEC.current_meta.get())
"#;

const CROSS_MODULE_ZEN: &str = r#"
load("@stdlib:v0.2.10/units.zen", "Voltage", "Current")

# Load the power module and access its metadata directly
load("./modules/PowerModule.zen", "SPEC")

# Test accessing metadata across module boundary
print("Loaded voltage metadata:", SPEC.voltage_meta.list())
print("Loaded current metadata:", SPEC.current_meta.list())

# Test that we can add more values to the loaded metadata
SPEC.voltage_meta.push(Voltage("24V"))
SPEC.current_meta.push(Current("2A"))

print("Updated voltage metadata:", SPEC.voltage_meta.list())
print("Updated current metadata:", SPEC.current_meta.list())
"#;

const POWER_INTERFACE_MODULE_ZEN: &str = r#"
load("@stdlib:v0.2.10/units.zen", "Voltage", "Current")

PowerSpec = interface(
    voltage_meta = metadata(Voltage),
    current_meta = metadata(Current),
)

voltage_rating = config("voltage_rating", str, default = "5V")
current_rating = config("current_rating", str, default = "1A")

POWER = io("POWER", PowerSpec)

# Initialize the interface metadata
POWER.voltage_meta.push(Voltage(voltage_rating))
POWER.current_meta.push(Current(current_rating))

print("Interface power module configured:", POWER.voltage_meta.get(), "at", POWER.current_meta.get())
"#;

const INTERFACE_CROSS_MODULE_ZEN: &str = r#"
load("@stdlib:v0.2.10/units.zen", "Voltage", "Current")

# Load the power interface module and access its interface
load("./modules/PowerInterfaceModule.zen", "POWER")

# Test accessing interface metadata across module boundary
print("Interface voltage metadata:", POWER.voltage_meta.list())
print("Interface current metadata:", POWER.current_meta.list())

# Create another interface instance with same metadata containers (testing shared state)
SharedPowerSpec = interface(
    voltage_meta = POWER.voltage_meta,
    current_meta = POWER.current_meta,
)

shared_power = SharedPowerSpec()

# Add values via the shared interface
shared_power.voltage_meta.push(Voltage("48V"))
shared_power.current_meta.push(Current("10A"))

print("Updated via shared interface - voltage:", POWER.voltage_meta.list())
print("Updated via shared interface - current:", POWER.current_meta.list())
"#;

const CONDITIONAL_BEHAVIOR_ZEN: &str = r#"
load("@stdlib:v0.2.10/units.zen", "Voltage")

# Create metadata container for tracking all voltages
voltage_tracker = metadata(Voltage)

# Add some voltage requirements
voltage_tracker.push(Voltage("3.3V"))
voltage_tracker.push(Voltage("5V"))
voltage_tracker.push(Voltage("12V"))

print("All voltages:", voltage_tracker.list())

# Conditional behavior based on metadata values
max_voltage = 0.0
high_voltage_present = False

for v in voltage_tracker.list():
    voltage_value = v.value  # Access numeric value directly
    print("Processing voltage value:", voltage_value)

    if voltage_value > max_voltage:
        max_voltage = voltage_value

    if voltage_value > 10.0:
        high_voltage_present = True

print("Maximum voltage found:", max_voltage, "V")

# Design decisions based on metadata analysis
if high_voltage_present:
    print("HIGH VOLTAGE DESIGN - adding isolation")
    voltage_tracker.push(Voltage("1000V"))  # Add isolation requirement
    print("Added isolation voltage:", voltage_tracker.get())
else:
    print("Low voltage design - standard components OK")

# Check total number of power domains
domain_count = len(voltage_tracker.list())
print("Total voltage domains:", domain_count)

if domain_count > 3:
    print("COMPLEX POWER DESIGN")
    print("Consider power management IC")
else:
    print("Simple power design")

print("Final voltage list:", voltage_tracker.list())
"#;

const BASIC_TYPES_METADATA_ZEN: &str = r#"
load("@stdlib:v0.2.10/units.zen", "Voltage")

# Test metadata with basic types
string_meta = metadata(str)
int_meta = metadata(int)
float_meta = metadata(float)

# Test with physical values
voltage_meta = metadata(Voltage)

print("=== Testing Basic Types ===")

# String metadata
string_meta.push("design_v1")
string_meta.push("design_v2")
string_meta.push("final_design")

print("String metadata:", string_meta.list())
print("Latest string:", string_meta.get())

# Integer metadata
int_meta.push(1)
int_meta.push(42)
int_meta.push(100)

print("Integer metadata:", int_meta.list())
print("Latest integer:", int_meta.get())

# Float metadata
float_meta.push(1.5)
float_meta.push(3.14)
float_meta.push(2.718)

print("Float metadata:", float_meta.list())
print("Latest float:", float_meta.get())

# Physical value metadata
voltage_meta.push(Voltage("3.3V"))
voltage_meta.push(Voltage("5V"))

print("Voltage metadata:", voltage_meta.list())
print("Latest voltage:", voltage_meta.get())

print("=== Conditional Logic with Basic Types ===")

# Conditional behavior with different types
total_designs = len(string_meta.list())
max_count = int_meta.get()
precision_factor = float_meta.get()

print("Total designs:", total_designs)
print("Max component count:", max_count)
print("Precision factor:", precision_factor)

if total_designs > 2:
    print("Multiple design iterations found")

if max_count > 50:
    print("High component count design")

if precision_factor > 3.0:
    print("High precision requirements")

# Mixed type analysis
design_complexity = total_designs * max_count * precision_factor
print("Design complexity score:", design_complexity)

if design_complexity > 500:
    print("COMPLEX DESIGN - consider modular approach")
else:
    print("Simple design - single module OK")
"#;

#[test]
fn test_simple_metadata_operations() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.10"])
        .write("SimpleMetadata.zen", SIMPLE_METADATA_ZEN)
        .snapshot_run("pcb", ["build", "SimpleMetadata.zen"]);
    assert_snapshot!("simple_metadata_operations", output);
}

#[test]
fn test_cross_module_metadata() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.10"])
        .write("modules/PowerModule.zen", POWER_MODULE_ZEN)
        .write("CrossModule.zen", CROSS_MODULE_ZEN)
        .snapshot_run("pcb", ["build", "CrossModule.zen"]);
    assert_snapshot!("cross_module_metadata", output);
}

#[test]
fn test_interface_cross_module_metadata() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.10"])
        .write(
            "modules/PowerInterfaceModule.zen",
            POWER_INTERFACE_MODULE_ZEN,
        )
        .write("InterfaceCrossModule.zen", INTERFACE_CROSS_MODULE_ZEN)
        .snapshot_run("pcb", ["build", "InterfaceCrossModule.zen"]);
    assert_snapshot!("interface_cross_module_metadata", output);
}

#[test]
fn test_conditional_metadata_behavior() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.10"])
        .write("ConditionalBehavior.zen", CONDITIONAL_BEHAVIOR_ZEN)
        .snapshot_run("pcb", ["build", "ConditionalBehavior.zen"]);
    assert_snapshot!("conditional_metadata_behavior", output);
}

#[test]
fn test_basic_types_metadata() {
    let output = Sandbox::new()
        .seed_stdlib(&["v0.2.10"])
        .write("BasicTypesMetadata.zen", BASIC_TYPES_METADATA_ZEN)
        .snapshot_run("pcb", ["build", "BasicTypesMetadata.zen"]);
    assert_snapshot!("basic_types_metadata", output);
}
