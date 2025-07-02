# Define module inputs/outputs
power = io("power", Net)
gnd = io("gnd", Net)

# Create two capacitors in this module
# First capacitor - decoupling cap
Component(
    name = "C1",
    type = "capacitor",
    footprint = "Capacitor_SMD:C_0402_1005Metric",
    pin_defs = {
        "P1": "1",
        "P2": "2",
    },
    pins = {
        "P1": power,
        "P2": gnd,
    },
    properties = {
        "package": "0402",
    },
)

# Second capacitor - bulk cap
Component(
    name = "C2",
    type = "capacitor",
    footprint = "Capacitor_SMD:C_0603_1608Metric",
    pin_defs = {
        "P1": "1",
        "P2": "2",
    },
    pins = {
        "P1": power,
        "P2": gnd,
    },
    properties = {
        "package": "0603",
    },
)

add_property("layout_path", "build/module_layout")
