# pcb-gltf

`pcb-gltf` writes a glTF 2.0 binary (GLB) for a `.kicad_pcb` file without
KiCad or OCCT. It backs `pcb gltf export`:

```bash
pcb gltf export board.kicad_pcb -o board.glb
```

It takes the arguments of `pcb step export`, as `kicad-cli pcb export glb`
takes those of `kicad-cli pcb export step`, except the copper flags: outer
copper is always exported, as KiCad's VRML export does. It exports the same
assembly: the board side comes from `pcb-step`'s scene, and every embedded
STEP model is tessellated with [foxtrot](https://github.com/diodeinc/foxtrot).
Models come from the board's embedded files; nothing is looked up on disk.

## What is written

- **Board**: one mesh, with a primitive per scene layer: body, copper, pads,
  vias, silkscreen and solder mask in KiCad's colours, read as sRGB as
  KiCad's VRML export reads them. Exposed copper, pads included, takes the
  colour of the stackup's copper finish (ENIG gold when none is named) and
  is not metallic, so it does not render black without an environment map.
  The board outline, cutouts and drills are chords within 0.005 mm, other
  arcs within 0.01 mm, and curved walls carry the
  exact normal of the arc or cylinder they lie on. Copper is flat, as in
  KiCad's VRML export: its top and bottom faces without walls, less the
  faces resting on the body and copper inside it. Every layer but the body
  is flat, so it is written without normals and loaders shade it flat.
- **Models**: one mesh per distinct embedded payload, with a primitive per
  STEP colour, drawn for every footprint that uses it with
  `EXT_mesh_gpu_instancing`; the node's `extras.references` lists the
  footprints in instance order. Models are tessellated within 0.01 mm in
  model units, the largest first, in parallel, then simplified within another
  0.01 mm of the placed model. Faces
  foxtrot cannot tessellate are left out with a warning; a model none of
  whose faces can be tessellated fails the export, as an unreadable model
  fails `pcb step export`.
- **Encoding**: vertices are ordered for the GPU's caches with
  [meshopt-rs](https://github.com/yzsolt/meshopt-rs), positions are snapped
  to a 16-bit grid per mesh and normals to 8 bits (`KHR_mesh_quantization`),
  and every vertex and index buffer is compressed with
  `EXT_meshopt_compression`. All three extensions are required; three.js
  reads the file once its `GLTFLoader` has a meshopt decoder
  (`loader.setMeshoptDecoder(MeshoptDecoder)`).
- **Frame**: the STEP frame converted to glTF's, metres and y up, by a root
  node. The origin options move it as they move the STEP output.

The output is deterministic.

## Model cache

Tessellating models is most of an export's time on boards with large
models. Set `PCB_GLTF_MODEL_CACHE_DIR` to a directory and `pcb gltf export`
keeps each tessellated model there, so the same STEP file on another board,
or on a later revision of the same board, is not tessellated again. It is
off when the variable is unset, and it never changes the output: the GLB
and the warnings are byte-for-byte those of an uncached export.

- **Keys.** An entry is addressed by the SHA-256 of the decoded STEP bytes,
  the simplification tolerance (which carries the model's scale), and a
  salt: the entry format, the CPU architecture, the sources of the
  tessellation and simplification code, and the locked versions of the
  crates they use (`build.rs` reads them from `Cargo.lock`). Changing any of
  them leaves old entries unused rather than wrong. Where the placement,
  the file name or the board differs, the entry is the same.
- **Entries** hold the simplified primitives, or the reason the model could
  not be tessellated, and how many faces failed. Warnings and the mesh name
  are rebuilt from the model that uses the entry, so they read as they do
  uncached.
- **Safety.** Entries are written to a unique temporary file and renamed
  into place, so readers see a whole entry or none and concurrent writers
  (threads, processes, containers sharing a volume) cannot corrupt one
  another. Each entry carries its key and a checksum; a truncated, damaged
  or misplaced entry is a miss and is rewritten. A directory that cannot be
  created disables the cache; one that cannot be written is read from only.
  Problems go to the log (`RUST_LOG=pcb_gltf=debug`, or `--debug`), not to
  the export's warnings.
- **Upkeep.** Nothing evicts entries or removes temporary files left by a
  killed process; delete the directory, or old files from it, whenever.
  Use a directory on local disk: the cache reads whole entries and is not
  built for network latency.

Embedders pass a [`ModelCache`] to `export_with_cache` and read its
`stats()`.
