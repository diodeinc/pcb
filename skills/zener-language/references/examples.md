# Examples

Component example: packaged bidirectional level shifter

```python
"""TXB0102DCUR - 2-Bit Bidirectional Voltage-Level Translator, Auto-Direction Sensing"""

Pins = struct(
    A1=io("A1", Net),
    A2=io("A2", Net),
    VCCA=io("VCCA", Net),
    GND=io("GND", Net),
    B2=io("B2", Net),
    B1=io("B1", Net),
    VCCB=io("VCCB", Net),
    OE=io("OE", Net),
)

Component(
    name="TXB0102DCUR",
    symbol=Symbol(library="TXB0102DCUR.kicad_sym"),
    footprint=File("VSSOP-8_2.3x2mm_P0.5mm.kicad_mod"),
    part=Part(mpn="TXB0102DCUR", manufacturer="Texas Instruments"),
    pins={
        "A1": Pins.A1,
        "A2": Pins.A2,
        "VCCA": Pins.VCCA,
        "GND": Pins.GND,
        "B2": Pins.B2,
        "B1": Pins.B1,
        "VCCB": Pins.VCCB,
        "OE": Pins.OE,
    },
    properties={
        "value": "TXB0102DCUR",
    },
)
```

Module example: LED helper module

```python
load("@stdlib/units.zen", "Voltage", "Resistance", "Current")
Led = Module("@stdlib/generics/Led.zen")
Resistor = Module("@stdlib/generics/Resistor.zen")

color = config(Led.Color, default="red")
supply_voltage = config(
    Voltage,
    default="3.3V",
    help="Expected drive supply used to size the series resistor",
)

parts = {
    "blue": {"vf": 2.8, "i_test": 0.005, "r": 110},
    "green": {"vf": 2.0, "i_test": 0.020, "r": 150},
    "orange": {"vf": 2.0, "i_test": 0.020, "r": 261},
    "red": {"vf": 2.0, "i_test": 0.020, "r": 150},
}

data = parts[color.value]
vf = Voltage(data["vf"])
i_test = Current(data["i_test"])
r_ohm = Resistance(data["r"])
i_led = (supply_voltage - vf) / r_ohm

check(vf < supply_voltage, "Forward voltage (%s) >= supply (%s)" % (vf, supply_voltage))
check(i_led >= 0, "Calculated LED current is negative")
check(i_led <= i_test, "LED current (%s) exceeds rated test current (%s)" % (i_led, i_test))

P1 = io(Net, help="LED drive input")
P2 = io(Net, help="LED cathode return")
_LED_R = Net("LED_R")

Led(
    name="D",
    package="0402",
    color=color,
    forward_voltage=vf,
    forward_current=i_led,
    A=_LED_R,
    K=P2,
)

Resistor(
    name="R_D",
    package="0402",
    value=r_ohm,
    P1=P1,
    P2=_LED_R,
)
```

Reference-design example: board-level composition excerpt

```python
load("@stdlib/interfaces.zen", "Swd", "Uart", "Usb2")
load("@stdlib/board_config.zen", "BoardConfig", "Constraints", "Copper", "DesignRules", "NetClass", "Silkscreen")

RP2040 = Module("github.com/dioderobot/demo/modules/RP2040.zen")
USB4105 = Module("github.com/dioderobot/demo/modules/USB4105.zen")
TLV758P = Module("github.com/dioderobot/demo/modules/TLV758P.zen")

VBUS_5V0 = Power(voltage="4.5 to 5.5V")
VDD_3V3 = Power(voltage="3.3V")
GND = Ground()
USB = Usb2()

usb_connector = USB4105(name="USB_C", mode="device", VBUS=VBUS_5V0, USB=USB, GND=GND)
ldo = TLV758P(name="LDO_3V3", input_voltage="5V", output_voltage="3.3V", VIN=VBUS_5V0, VOUT=VDD_3V3, GND=GND)
mcu = RP2040(name="MCU", VDD_3V3=VDD_3V3, GND=GND, USB=USB)

Board(
    name="DM0002",
    layers=4,
    layout_path="layout/DM0002",
)
```
