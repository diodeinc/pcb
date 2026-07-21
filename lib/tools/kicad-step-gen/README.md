# KiCad STEP generation tools

These scripts regenerate the compact STEP models derived from KiCad pin-header
and pin-socket generator geometry.

The scripts are GPL-3.0-or-later unless otherwise noted because they import
KiCad's `kicad-footprint-generator` package. They are separate from the
MIT-licensed project code. See `LICENSE.md`.

Run these commands from the repository root:

```bash
cargo build -p pcbc
lib/tools/kicad-step-gen/generate_pinheader_step.py --embed
lib/tools/kicad-step-gen/generate_pinsocket_step.py --embed
```
