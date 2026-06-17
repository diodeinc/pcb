# Third-Party Notices

The root `LICENSE` covers Diode-authored code and docs. Third-party files keep
their own licenses.

## KiCad Libraries

Selected files from the KiCad
[kicad-symbols](https://gitlab.com/kicad/libraries/kicad-symbols) and
[kicad-footprints](https://gitlab.com/kicad/libraries/kicad-footprints)
repositories are redistributed as part of `lib/std`; selected STEP model data
from [kicad-packages3D](https://gitlab.com/kicad/libraries/kicad-packages3D)
and regenerated pin-header and pin-socket STEP models are embedded into
footprint files. These files remain under
`CC-BY-SA-4.0 WITH KiCad-libraries-exception`. See the
[KiCad library license](https://www.kicad.org/libraries/license/),
[CC BY-SA 4.0 legal code](https://creativecommons.org/licenses/by-sa/4.0/legalcode),
and [KiCad Libraries Exception](https://spdx.org/licenses/KiCad-libraries-exception.html).
The upstream license text is included at `lib/std/kicad-symbols/LICENSE.md`
and `lib/std/kicad-footprints/LICENSE.md`.

### KiCad Library Files

| Local path | Changes |
| --- | --- |
| `lib/std/kicad-footprints/Button_Switch_SMD.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Capacitor_SMD.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Connector.pretty/` | Selected files only; unmodified |
| `lib/std/kicad-footprints/Connector_JST.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Connector_PinHeader_1.27mm.pretty/` | Selected files only; embedded STEP models |
| `lib/std/kicad-footprints/Connector_PinHeader_2.54mm.pretty/` | Selected files only; embedded STEP models |
| `lib/std/kicad-footprints/Connector_PinSocket_1.27mm.pretty/` | Selected files only; embedded STEP models |
| `lib/std/kicad-footprints/Connector_PinSocket_2.54mm.pretty/` | Selected files only; embedded STEP models |
| `lib/std/kicad-footprints/Connector_USB.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Crystal.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Diode_SMD.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Fiducial.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Inductor_SMD.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Jumper.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/LED_SMD.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/MountingHole.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/NetTie.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Oscillator.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Package_DFN_QFN.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Package_SO.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Package_SON.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/Resistor_SMD.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-footprints/TestPoint.pretty/` | Selected files only; embedded referenced STEP models |
| `lib/std/kicad-symbols/Connector.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Connector_Generic.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Connector_Generic_MountingPin.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Device.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/MCU_RaspberryPi.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Mechanical.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Memory_Flash.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Oscillator.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Power_Protection.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Regulator_Linear.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Simulation_SPICE.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Switch.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/power.kicad_symdir/` | Selected files only; unmodified |
| `lib/std/kicad-symbols/Simulation_SPICE.sp` | Unmodified |

## ruff

`pcb fmt` uses formatter code from [ruff](https://github.com/astral-sh/ruff),
licensed under MIT. See the
[ruff license text](https://github.com/astral-sh/ruff/blob/main/LICENSE).

## KiCad Footprint Generator

`lib/tools/kicad-step-gen/` contains STEP regeneration tooling that imports
[kicad-footprint-generator](https://gitlab.com/kicad/libraries/kicad-footprint-generator),
which is GPL-3.0-or-later unless otherwise noted upstream. The local tooling is
distributed under GPL-3.0-or-later; see `lib/tools/kicad-step-gen/LICENSE.md`.
