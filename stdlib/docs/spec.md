# Zen Standard Library Specification

## Quick Start

```zen
load("@stdlib/interfaces.zen", "Ground", "Power")


Resistor = Module("@stdlib/generics/Resistor.zen")
Capacitor = Module("@stdlib/generics/Capacitor.zen")
Inductor = Module("@stdlib/generics/Inductor.zen")

vcc = Power("3V3")
gnd = Ground("GND")

# LED with current limiting resistor
Resistor("R1", "220Ohm", "0603", P1=vcc.NET, P2=Net("LED_A"))
Led("D1", color="green", package="0603", A=Net("LED_A"), K=gnd.NET)
Capacitor("C1", "100nF", "0402", voltage="16V", P1=vcc.NET, P2=gnd.NET)
```

## Core Concepts

**Nets**: `Net("name")`, `Power("VCC")`, `Ground("GND")`, `Gpio("GPIO1")`, `NotConnected()`  
**Access**: Use `.NET` on interface nets: `VCC.NET`, `gnd.NET`  
**Loading**: `load("@stdlib/interfaces.zen", "Power", "Ground", "Spi")`  
**Generics**: `Generic = Module("@stdlib/generics/<Generic>.zen")`  
**Naming Convention**: Net variables should be CAPITALIZED (e.g., `UART_TX`, `LED_CTRL`), while interface variables should be lowercase (e.g., `uart`, `spi`, `i2c`)

## Units

| Unit        | Format         | Examples                   |
| ----------- | -------------- | -------------------------- |
| Resistance  | `"XXOhm[ Y%]"` | `"10kOhm"`, `"4.7MOhm 1%"` |
| Capacitance | `"XXF[ Y%]"`   | `"100nF"`, `"10uF 20%"`    |
| Voltage     | `"XXV[ Y%]"`   | `"3.3V"`, `"12V 0.5%"`     |
| Current     | `"XXA[ Y%]"`   | `"20mA"`, `"1A"`           |
| Inductance  | `"XXH[ Y%]"`   | `"10uH"`, `"100mH"`        |
| Frequency   | `"XXHz[ Y%]"`  | `"8MHz"`, `"32.768kHz"`    |
| Temperature | `"XXC"`        | `"25C"`, `"-40C"`          |
| Time        | `"XXs"`        | `"1ms"`, `"100ns"`         |

**Prefixes**: Y, Z, E, P, T, G, M, k, m, µ/u, n, p, f, a, z, y

## Components

### Resistor

| Param   | Type | Required | Values                                                         |
| ------- | ---- | -------- | -------------------------------------------------------------- |
| value   | str  | ✓        | `"1Ohm"-"10MOhm"` with optional `" X%"`                        |
| package | str  | ✓        | `0201`, `0402`, `0603`, `0805`, `1206`, `1210`, `2010`, `2512` |
| voltage | str  | ✗        | e.g. `"50V"`                                                   |

```zen
Resistor = Module("@stdlib/generics/Resistor.zen")

Resistor("R1", "10kOhm", "0603", P1=vcc.NET, P2=gnd.NET)
Resistor("R2", "4.7kOhm 1%", "0402", voltage="50V", P1=SIG, P2=gnd.NET)
```

### Capacitor

| Param      | Type | Required | Values                                                 |
| ---------- | ---- | -------- | ------------------------------------------------------ |
| value      | str  | ✓        | `"1pF"-"1000uF"` with optional `" X%"`                 |
| package    | str  | ✓        | `0201`, `0402`, `0603`, `0805`, `1206`, `1210`         |
| voltage    | str  | ✗        | e.g. `"16V"`                                           |
| dielectric | str  | ✗        | `C0G`, `NP0`, `X5R`, `X7R`, `X7S`, `X7T`, `Y5V`, `Z5U` |
| esr        | str  | ✗        | e.g. `"100mOhm"`                                       |

```zen
Capacitor = Module("@stdlib/generics/Capacitor.zen")

Capacitor("C1", "100nF", "0402", voltage="16V", dielectric="X7R", P1=vcc.NET, P2=gnd.NET)
Capacitor("C2", "10uF 20%", "1206", voltage="25V", P1=vin.NET, P2=gnd.NET)
```

### Inductor

| Param   | Type | Required | Values                             |
| ------- | ---- | -------- | ---------------------------------- |
| value   | str  | ✓        | `"1nH"-"1H"` with optional `" X%"` |
| package | str  | ✓        | `0201`-`2512`                      |
| current | str  | ✗        | e.g. `"1A"`                        |

```zen
Inductor = Module("@stdlib/generics/Inductor.zen")

Inductor("L1", "10uH", "0805", current="1A", P1=SW, P2=VOUT)
```

### FerriteBead

| Param      | Type | Required | Values                 |
| ---------- | ---- | -------- | ---------------------- |
| resistance | str  | ✓        | Impedance at frequency |
| frequency  | str  | ✓        | Rated frequency        |
| package    | str  | ✓        | `0402`-`1812`          |
| current    | str  | ✗        | Current rating         |

```zen
FerriteBead = Module("@stdlib/generics/FerriteBead.zen")

FerriteBead("FB1", resistance="100Ohm", frequency="100MHz", package="0603", P1=VCC_IN, P2=VCC_OUT)
```

### Led

| Param           | Type | Required | Values                                                                            |
| --------------- | ---- | -------- | --------------------------------------------------------------------------------- |
| color           | str  | ✓        | `red`, `green`, `blue`, `yellow`, `white`, `amber`, `orange`, `pink`, `uv`, `rgb` |
| package         | str  | ✓        | `0201`-`2512`, `1210_Reverse`, `1812`                                             |
| forward_voltage | str  | ✗        | e.g. `"2.1V"`                                                                     |
| forward_current | str  | ✗        | e.g. `"20mA"`                                                                     |

```zen
Led = Module("@stdlib/generics/Led.zen")

Led("D1", color="green", package="0603", A=LED_CTRL, K=gnd.NET)
Led("D2", color="red", package="0402", forward_voltage="2.0V", A=SIG, K=gnd.NET)
```

### Bjt

| Param    | Type  | Required | Values                                    |
| -------- | ----- | -------- | ----------------------------------------- |
| bjt_type | str   | ✓        | `NPN`, `PNP`                              |
| package  | str   | ✓        | `SOT-23-3`, `SOT-323`, `SOT-523`, `SC-70` |
| hfe      | float | ✗        | Current gain                              |
| vceo     | str   | ✗        | Collector-emitter voltage                 |
| ic_max   | str   | ✗        | Max collector current                     |

```zen
Bjt = Module("@stdlib/generics/Bjt.zen")

Bjt("Q1", bjt_type="NPN", package="SOT-23-3", COLLECTOR=LOAD, BASE=CTRL, EMITTER=gnd.NET)
Bjt("Q2", bjt_type="PNP", package="SOT-23-3", hfe=100.0, vceo="30V", COLLECTOR=OUT, BASE=IN, EMITTER=VCC)
```

### Mosfet

| Param   | Type | Required | Values               |
| ------- | ---- | -------- | -------------------- |
| channel | str  | ✓        | `N`, `P`             |
| package | str  | ✓        | `SOT-23-3`, etc.     |
| vds_max | str  | ✗        | Drain-source voltage |
| id_max  | str  | ✗        | Drain current        |
| rds_on  | str  | ✗        | On-resistance        |

```zen
Mosfet = Module("@stdlib/generics/Mosfet.zen")

Mosfet("M1", channel="N", package="SOT-23-3", GATE=CTRL, DRAIN=LOAD, SOURCE=gnd.NET)
Mosfet("M2", channel="P", package="SOT-23-3", vds_max="20V", GATE=EN, DRAIN=OUT, SOURCE=VCC)
```

### Crystal

| Param            | Type | Required | Values                                                          |
| ---------------- | ---- | -------- | --------------------------------------------------------------- |
| frequency        | str  | ✓        | Oscillation frequency                                           |
| load_capacitance | str  | ✓        | Load capacitance                                                |
| package          | str  | ✓        | `2016_4Pin`, `2520_4Pin`, `3225_4Pin`, `5032_4Pin`, `7050_4Pin` |
| tolerance        | str  | ✗        | e.g. `"20ppm"`                                                  |
| esr              | str  | ✗        | Series resistance                                               |

```zen
Crystal = Module("@stdlib/generics/Crystal.zen")

Crystal("X1", frequency="16MHz", load_capacitance="18pF", package="5032_4Pin", XIN=MCU_XI, XOUT=MCU_XO, GND=gnd.NET)
Crystal("X2", frequency="32.768kHz", load_capacitance="12.5pF", package="3225_4Pin", XIN=RTC_XI, XOUT=RTC_XO, GND=gnd.NET)
```

### TestPoint

| Param   | Type | Required | Values                                         |
| ------- | ---- | -------- | ---------------------------------------------- |
| variant | str  | ✓        | `Pad_D1.0mm`, `Pad_D1.5mm`, `Pad_D2.0mm`, etc. |

```zen
TestPoint = Module("@stdlib/generics/TestPoint.zen")

TestPoint("TP1", variant="Pad_D1.5mm", P1=VCC_3V3)
TestPoint("TP2", variant="Pad_D1.0mm", P1=DEBUG_TX)
```

### MountingHole

| Param    | Type | Required | Values                               |
| -------- | ---- | -------- | ------------------------------------ |
| diameter | str  | ✓        | `M2`, `M2.5`, `M3`, `M4`, `M5`, `M6` |
| standard | str  | ✓        | `DIN965`, `ISO7380`, `ISO14579`      |
| plating  | str  | ✓        | `None`, `TopBottom`                  |

```zen
MountingHole = Module("@stdlib/generics/MountingHole.zen")

MountingHole("MH1", diameter="M3", standard="DIN965", plating="TopBottom")
```

### SolderJumper

| Param     | Type | Required | Values                                                                    |
| --------- | ---- | -------- | ------------------------------------------------------------------------- |
| style     | str  | ✓        | `Open`, `Bridged` (2-pin); `Bridged12`, `Bridged23`, `Bridged123` (3-pin) |
| variant   | str  | ✓        | `Pad`, `Arrow`                                                            |
| pitch     | str  | ✓        | `P0.8mm`, `P1.0mm`, `P1.3mm`                                              |
| pin_count | int  | ✗        | `2` (default) or `3`                                                      |

```zen
SolderJumper = Module("@stdlib/generics/SolderJumper.zen")

SolderJumper("SJ1", style="Open", variant="Pad", pitch="P1.3mm", P1=OPT_A, P2=OPT_B)
SolderJumper("SJ2", pin_count=3, style="Bridged12", variant="Pad", pitch="P1.3mm", P1=NET_A, P2=NET_COM, P3=NET_B)
```

### NetTie

| Param     | Type | Required | Values         |
| --------- | ---- | -------- | -------------- |
| pad_size  | str  | ✓        | e.g. `"0.5mm"` |
| pin_count | int  | ✓        | `2` or `3`     |

```zen
NetTie = Module("@stdlib/generics/NetTie.zen")

NetTie("NT1", pad_size="0.5mm", pin_count=2, P1=AGND, P2=DGND)
```

### PinSocket

| Param       | Type | Required | Values                                   |
| ----------- | ---- | -------- | ---------------------------------------- |
| pins        | int  | ✓        | `1`-`40`                                 |
| rows        | int  | ✓        | `1` or `2`                               |
| pitch       | str  | ✓        | `2.00mm`, `2.54mm`                       |
| orientation | str  | ✓        | `Vertical`, `Horizontal`, `Vertical_SMD` |

```zen
PinSocket = Module("@stdlib/kicad/PinSocket.zen")

PinSocket("J1", pins=6, rows=1, pitch="2.54mm", orientation="Vertical", P1=VCC, P2=gnd.NET, P3=UART_TX, P4=UART_RX, P5=RST_N, P6=BOOT_MODE)
PinSocket("J2", pins=10, rows=2, pitch="2.54mm", orientation="Vertical", P1=VCC, P2=gnd.NET, ...) # P1-P20
```

### SolderWire

| Param      | Type | Required | Values          |
| ---------- | ---- | -------- | --------------- |
| awg        | str  | ✓        | `AWG30`-`AWG8`  |
| pad_size   | str  | ✓        | `1.0mm`-`5.0mm` |
| drill_size | str  | ✓        | `0.5mm`-`3.0mm` |

```zen
SolderWire = Module("@stdlib/kicad/SolderWire.zen")

SolderWire("W1", awg="AWG22", pad_size="2.0mm", drill_size="1.0mm", P1=VIN)
```

### TagConnect

| Param    | Type | Required | Values          |
| -------- | ---- | -------- | --------------- |
| tag_type | str  | ✓        | See table below |

Types: `TC2030-IDC-NL-2x03` (6-pin), `TC2050-IDC-FP-2x05` (10-pin), `TC2070-IDC-FP-2x07` (14-pin)  
Suffixes: `-NL` = no legs, `-FP` = with footprint, `-BottomClip` = clip compatible

```zen
TagConnect = Module("@stdlib/kicad/TagConnect.zen")

TagConnect("J1", tag_type="TC2030-IDC-NL-2x03", P1=VCC, P2=SWDIO, P3=RST, P4=SWCLK, P5=gnd.NET, P6=SWO)
```

### MolexPicoBlade

| Param   | Type | Required | Values                          |
| ------- | ---- | -------- | ------------------------------- |
| variant | str  | ✓        | e.g. `53047-0410_1x04_Vertical` |

Format: `[PartNumber]_[Pins]_[Orientation]` where Pins=`1x02`-`1x17`, Orientation=`Vertical`/`Horizontal`

```zen
MolexPicoBlade = Module("@stdlib/kicad/MolexPicoBlade.zen")

MolexPicoBlade("J1", variant="53047-0410_1x04_Vertical", P1=VCC, P2=gnd.NET, P3=I2C_SDA, P4=I2C_SCL)
```

## Interfaces

```zen
load("@stdlib/interfaces.zen", "Uart", "I2c", "Spi", "Power", "Ground")

# Basic interfaces
uart = Uart(TX=Net("UART_TX"), RX=Net("UART_RX"))
i2c = I2c(SDA=Net("I2C_SDA"), SCL=Net("I2C_SCL"))
spi = Spi(CS=Net("SPI_CS"), MISO=Net("SPI_MISO"), MOSI=Net("SPI_MOSI"), CLK=Net("SPI_CLK"))

# Interface pairs
(uart_a, uart_b) = UartPair("MCU", "FTDI")  # Creates cross-connected UARTs
```

Available: `Analog`, `Can`, `DiffPair`, `Ethernet`, `Gpio`, `Hdmi`, `I2s`, `I3c`, `Jtag`, `Lcd`, `Lvds`, `Mdio`, `Mii`, `Mipi`, `OneWire`, `OscPair`, `Pcie`, `Pwm`, `Qspi`, `Rgmii`, `Rmii`, `Sata`, `Sdio`, `Sdmmc`, `Swd`, `Usb2`, `Usb3`

## Configuration

```zen
load("@stdlib/config.zen", "config", "config_unit")

# Basic config
package = config("package", str, default="0603")
value = config_unit("value", Resistance)  # Unit-aware config

# Property generation
properties = {
    "resistance": "10kOhm",
    "voltage": "50V",
}
```

## Pin Mapping

```zen
load("@stdlib/pins.zen", "PinMapBuilder", "PinInterface")

# Define pin interfaces
SPI1 = PinInterface("SPI1", Spi)
pin_map = {
    "PA5": [SPI1.clk],
    "PA6": [SPI1.miso],
    "PA7": [SPI1.mosi],
}

# Build connections
builder = PinMapBuilder(pin_map)
spi_bus = Spi(CLK=Net("SPI_CLK"), MISO=Net("SPI_MISO"), MOSI=Net("SPI_MOSI"), CS=Net("SPI_CS"))
builder.assign("SPI1", spi_bus, clk="PA5", miso="PA6", mosi="PA7")
pins = builder.build()
```

## Custom Components

```zen
load("@stdlib/config.zen", "config_unit")
load("@stdlib/units.zen", "Capacitance")

# Define parameters
Package = enum("0402", "0603", "0805")
package = config("package", Package)
value = config_unit("value", Capacitance)

# Define nets
P1 = io("P1", Net)
P2 = io("P2", Net)

# Define component
Component(
    name = "MY_CAP",
    type = "capacitor",
    prefix = "C",
    symbol = Symbol(library = "@kicad-symbols/Device.kicad_sym", name = "C"),
    footprint = File("@kicad-footprints/Capacitor_SMD.pretty/C_0603_1608Metric.kicad_mod"),
    properties = {"capacitance": value, "package": package},
    pins = {"P1": P1, "P2": P2},
)
```
