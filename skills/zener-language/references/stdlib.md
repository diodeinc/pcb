# Stdlib Reference

Prelude:

- Stdlib prelude symbols available in normal user `.zen` files without `load()`: `Net`, `Power`, `Ground`, `NotConnected`, `Board`, `Layout`, `Part`.
- Local definitions can shadow prelude symbols.

High-value stdlib surface:

`@stdlib/interfaces.zen`

- Common specialized nets include `Analog`, `Gpio`, and `Pwm`.
- Common interfaces include `DiffPair`, `I2c`, `I3c`, `Spi`, `Qspi`, `Uart`, `Usart`, `Swd`, `Jtag`, `Usb2`, `Usb3`, and others.
- `UartPair()` and `UsartPair()` generate cross-connected point-to-point links.

`@stdlib/units.zen`

- High-value physical types: `Voltage`, `Current`, `Resistance`, `Capacitance`, `Inductance`, `Impedance`, `Frequency`, `Temperature`, `Time`, and `Power`.
- Constructors accept strings, ranges, and tolerances.
- Arithmetic preserves units, so code can compute currents, resistor dividers, and power directly.
- For supported properties, methods, operators, formatting, and range behavior, read the Physical Quantities section in `~/.pcb/docs/spec.md`.

`@stdlib/checks.zen`

- `voltage_within(...)` is the main reusable `io()`-boundary power-rail check.

`@stdlib/utils.zen`

- `e3`, `e6`, `e12`, `e24`, `e48`, `e96`, `e192` snap physical values to standard E-series values.
- `format_value(...)` helps compose readable property strings.

`@stdlib/properties.zen`

- `Layout(...)` associates a layout path to the module.

`@stdlib/generics/*`

- Prefer generics for common parts: `Resistor`, `Capacitor`, `Inductor`, `FerriteBead`, `Led`, `Diode`, `Tvs`, `Crystal`, `Thermistor`, `TestPoint`, `PinHeader`, `TerminalBlock`, `NetTie`, `SolderJumper`, `MountingHole`, `Standoff`, and `Version`.
- Read package docs for each generic's accepted parameters and behavior.

More detail:

- For stdlib and package docs use `pcb doc --package @stdlib`, `pcb doc --package @stdlib/generics`, or `pcb doc --package <package>`.
