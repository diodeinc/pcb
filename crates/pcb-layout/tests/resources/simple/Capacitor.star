load("units.star", "Capacitance")

Package = enum("0201", "0402", "0603", "0805", "1206", "1210", "2010", "2512")

package = config("package", Package, convert = Package)
value = config("value", Capacitance, default = Capacitance(value = 0.0), convert = lambda x: Capacitance(value = x))
P1 = io("P1", Net)
P2 = io("P2", Net)

_footprints = {
    Package("0201"): "Capacitor_SMD:C_0201_0603Metric",
    Package("0402"): "Capacitor_SMD:C_0402_1005Metric",
    Package("0603"): "Capacitor_SMD:C_0603_1608Metric",
    Package("0805"): "Capacitor_SMD:C_0805_2012Metric",
}

Component(
    name = "C",
    type = "capacitor",
    footprint = _footprints[package],
    pin_defs = {
        "P1": "1",
        "P2": "2",
    },
    pins = {
        "P1": P1,
        "P2": P2,
    },
    properties = {
        "value": value,
        "package": package,
    },
)
