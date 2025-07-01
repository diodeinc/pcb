load(".", "Capacitor")
load("eda", _BMI270 = "BMI270")
load("stdlib.star", "Ground", "I2C", "Power")
load("units.star", "Capacitance")

power = io("power", Power)
i2c = io("i2c", I2C)

_BMI270(
    name = "IC",
    pins = {
        "VDD": power.vcc,
        "GND": Ground,
        "SCX": i2c.scl,
        "SDX": i2c.sda,
        "ASCX": i2c.scl,
        "ASDX": i2c.sda,
        "CSB": Net(),
        "GNDIO": Ground,
        "INT1": Net(),
        "INT2": Net(),
        "OCSB": Net(),
        "OSDO": Net(),
        "SDO": Net(),
        "VDDIO": power.vcc,
    },
    footprint = "eda/BMI270.kicad_mod",
)

Capacitor(
    name = "C",
    P1 = power.vcc,
    P2 = Ground,
    package = "0402",
    value = 0.0,
)
