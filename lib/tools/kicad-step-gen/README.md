# KiCad STEP Generation Tools

This directory contains regeneration tooling for compact STEP models derived
from KiCad pin-header and pin-socket generator geometry. It is separate from
the MIT-licensed project code.

Scripts here are GPL-3.0-or-later unless otherwise noted because they import
KiCad's `kicad-footprint-generator` package. See `LICENSE.md`.

```bash
cargo build -p pcbc
lib/tools/kicad-step-gen/generate_pinheader_step.py --embed
lib/tools/kicad-step-gen/generate_pinsocket_step.py --embed
```
