# pcb-gltf

`pcb-gltf` writes a glTF 2.0 binary (GLB) for a `.kicad_pcb` file without
KiCad or OCCT. It backs `pcb gltf export`:

```bash
pcb gltf export board.kicad_pcb -o board.glb
```

It takes the arguments of `pcb step export`, as `kicad-cli pcb export glb`
takes those of `kicad-cli pcb export step`, except the copper flags: outer
copper is exported, as KiCad's VRML export does, and inner copper with
`--include-inner-copper`. It exports the same assembly: the board side
comes from `pcb-step`'s scene, and every STEP model is tessellated with
[foxtrot](https://github.com/diodeinc/foxtrot).

## What is written

- **Board**: one mesh, with a primitive per scene layer: body, copper, pads,
  vias, solder paste, silkscreen and solder mask in KiCad's colours, read as
  sRGB as KiCad's VRML export reads them. The mask is translucent (17 %) and
  the silkscreen and paste opaque, as in that export. Copper is trimmed to
  the board outline, and vias that reach a surface are bored through the
  body and the mask, as in that export. Exposed copper, pads included,
  takes the colour of the stackup's copper finish (ENIG gold when none is named) and
  is not metallic, so it does not render black without an environment map.
  The board outline, cutouts and drills are chords within 0.005 mm, other
  arcs within 0.01 mm, and curved walls carry the
  exact normal of the arc or cylinder they lie on. Copper is flat, as in
  KiCad's VRML export: its top and bottom faces without walls, less the
  faces resting on the body and copper inside it. Every layer but the body
  is flat, so it is written without normals and loaders shade it flat.
- **Models**: one mesh per distinct model payload, with a primitive per
  material, drawn for every footprint that uses it with
  `EXT_mesh_gpu_instancing`; the node's `extras.references` lists the
  footprints in instance order. Models are tessellated within 0.01 mm in
  model units, the largest first, in parallel, then simplified within another
  0.01 mm of the placed model. Faces
  foxtrot cannot tessellate are left out with a warning; a model none of
  whose faces can be tessellated fails the export, as an unreadable model
  fails `pcb step export`.
- **Encoding**: vertices are ordered for the GPU's caches with
  [meshopt-rs](https://github.com/yzsolt/meshopt-rs), positions are snapped
  to a 16-bit grid per mesh (the board's height on a grid of its own, so a
  metre-wide board keeps its 0.015 mm mask-to-copper gap) and normals to 8
  bits (`KHR_mesh_quantization`),
  and every vertex and index buffer is compressed with
  `EXT_meshopt_compression`. All three extensions are required; three.js
  reads the file once its `GLTFLoader` has a meshopt decoder
  (`loader.setMeshoptDecoder(MeshoptDecoder)`).
- **Frame**: the STEP frame converted to glTF's, metres and y up, by a root
  node. The origin options move it as they move the STEP output.

The output is deterministic.

## STEP appearance

STEP surface styles supply appearance, not a metal/plastic classification.
Materials with the same RGB but different opacity, diffuse strength, specular
properties or shininess remain separate through tessellation and optimization. Payload
deduplication hashes the complete model bytes, including its styles.

- Foxtrot supplies authored surface RGB and optional diffuse reflectance.
  RGB is interpreted as sRGB, converted to linear and multiplied by diffuse
  reflectance (one when absent) to produce `baseColorFactor`.
  This is the exporter's existing convention; STEP `COLOUR_RGB` does not
  declare a color space. The same convention applies to specular color.
- Opacity is `1 - transparency`, without gamma conversion. Opacity below
  one selects `alphaMode: BLEND`. This is surface transparency, not inferred
  glass transmission, refraction or volume absorption.
- Authored specular color and strength use
  [`KHR_materials_specular`](https://github.com/KhronosGroup/glTF/tree/main/extensions/2.0/Khronos/KHR_materials_specular).
  The approximation treats `strength × linear(specular color)` as normal-incidence
  reflectance (F0). A missing color uses white; a missing strength uses one.
  `specularFactor` holds strength and `specularColorFactor` holds linear color
  divided by 0.04, cancelling glTF's default dielectric F0. The extension
  permits color factors above one, but strength must stay in [0, 1]. It is
  listed in `extensionsUsed`, not `extensionsRequired`: readers without it
  retain base color, opacity and roughness but lose the authored specular response.
- The raw Phong exponent `n` maps to perceptual roughness as
  `(2 / (4*n + 2))^(1/4)`. This first approximates Phong with a Blinn exponent
  of [`4*n`](https://learnopengl.com/Advanced-Lighting/Advanced-Lighting), then uses the
  [NDF parameterization](https://graphicrants.blogspot.com/2013/08/specular-brdf-reference.html)
  `alpha = roughness²`. Phong and GGX have different lobes and energy models:
  this preserves shininess ordering, not an exact match to a CAD renderer.
- Missing shininess retains roughness 0.5. Missing both specular fields
  omits the extension and retains glTF's neutral dielectric response.
  Metallic remains zero; ambient coefficients and shading-method presets
  are not mapped. Color-only STEP, VRML, proxies and board-layer defaults
  are otherwise unchanged. Open shells remain double-sided.

## Finding models

A footprint names a model as `kicad-embed://name`, embedded in the board, or
as a path that may use variables such as `${KIPRJMOD}`. Embedded models are
always read. A board is untrusted input, so a path is read only through a
`ModelResolver` built from directories the caller chose: variables come
from the resolver, never the environment; the canonical path (symlinks
followed) must lie under an allowed directory; and only regular `.step`,
`.stp` and `.wrl` files up to 256 MiB are read. A path that escapes is
`absent`, like a file that does not exist. The default resolver reads only
embedded models. On the command line: `--project-models` (`${KIPRJMOD}`),
`--model-var NAME=DIR` and `--model-dir DIR`.

VRML 2.0 models are read as KiCad reads them (`IndexedFaceSet` and `Box`,
in 0.1 inch units); anything else in the file is reported as not read.
Non-uniform model scales are kept as the instance's glTF `scale`; `pcb step
export` cannot place them, nor VRML models, and reports them `unsupported`.

## What was drawn

Warnings and an exit code cannot say which components are missing, so each
model a footprint names has a status, in `Report::coverage`, `--coverage
FILE` and the GLB's manifest: `rendered`, `hidden`, `filtered` (by the
options), `absent`, `unsupported` or `failed`. The last three are missing:
`--require-complete` fails the command, after writing the file, if any
model is. `--proxy-missing-models` draws a box over the pads of each
footprint with a missing model; `--footprint-only` draws one over every
footprint and reads no models.

A missing or open `Edge.Cuts` is an error, as manufacturing output needs a
defined shape. For viewing unfinished boards, `--synthesize-outline` gives
the body the rounded convex hull of what is on the board, with a warning.

## Scene manifest

The root node's `extras.pcb` holds a manifest, schema `pcb-scene/1`, in the
scene's millimetres: each footprint's reference, uuid, library id, value,
side, position, rotation, DNP, mount type, BOM and position-file
exclusions, nets, whether it has a proxy, and its models' statuses with the
`instance: {node, index}` that draws each; and the board's thickness,
copper layers and their heights, and net names. Each model node's
`extras.references` names the footprint of each instance, so a picked
`instanceId` maps to a reference. Each board primitive's `extras` names its
layer and side.

## Depth

The layers are separate surfaces: mask 0.015 mm above the copper, paste
0.025 mm and silkscreen 0.04 mm above it. A viewer must keep them apart: a
24-bit depth buffer resolves about `z² / (near · 2²⁴)` at distance `z`, so
fit `near` and `far` to the board (`near ≥ 0.01 · extent`), or use a
reversed-Z depth buffer.

## Differences from KiCad's VRML export

Body, mask and silkscreen areas agree with `kicad-cli pcb export vrml`
within 0.1 % at the median over 76 corpus boards. The exceptions: a panel
whose routed gaps touch keeps them filled in KiCad; paste of pads with a
negative margin beyond the pad's size is a fixed aperture in KiCad;
silkscreen in fonts the board does not embed uses a system font in KiCad
and the stroke font here; and text on copper layers is not copper here.
