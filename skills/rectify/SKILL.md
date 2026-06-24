---
name: rectify
description: Use when validating or modifying KiCad footprints that have embedded or referenced STEP/3D models. Checks whether the footprint model transform lines up component pins/pads with the footprint pads/holes, and patches `(rotate ...)` / `(offset ...)` when needed.
---

# Rectify

`rectify` is a standalone CLI for checking and fixing KiCad footprint 3D model alignment. It compares a footprint's pads/holes against the tessellated STEP geometry embedded in the footprint and infers the best `(rotate ...)` / `(offset ...)` model transform.

Use this for every new or modified `.kicad_mod` that includes a 3D model, especially after importing ECAD artifacts or embedding a STEP model with `pcb embed-step`.

## Commands

Check whether stored transforms look wrong:

```bash
pcb rectify check <footprint.kicad_mod>
```

Patch the footprint in place when audit flags a wrong-looking transform:

```bash
pcb rectify fix <footprint.kicad_mod>
```

Preview the legacy patch path without writing:

```bash
rectify patch <footprint.kicad_mod> --dry-run
```

Inspect the top inferred pose as JSON:

```bash
rectify solve <footprint.kicad_mod>
```

## Workflow

1. Ensure the footprint has a trusted embedded STEP model.
2. Run `pcb rectify check <footprint.kicad_mod>`.
3. If audit flags the footprint, run `pcb rectify fix <footprint.kicad_mod>`.
4. Re-run `pcb rectify check <footprint.kicad_mod>` to confirm it is clean.
5. If it still fails after patching, the model geometry likely does not match the footprint; report the mismatch instead of forcing a transform.

Do not use `rectify` as a substitute for datasheet footprint validation. It checks the model-to-footprint transform, not whether the footprint itself matches the manufacturer package drawing.
