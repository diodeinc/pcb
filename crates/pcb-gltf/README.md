# pcb-gltf

`pcb-gltf` writes a glTF 2.0 binary (GLB) for a `.kicad_pcb` file without
KiCad or OCCT. It backs `pcb gltf export`:

```bash
pcb gltf export board.kicad_pcb -o board.glb
```

It takes the same arguments as `pcb step export`, as `kicad-cli pcb export
glb` takes those of `kicad-cli pcb export step`, and exports the same
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
  arcs and round holes within 0.01 mm, and curved walls carry the
  exact normal of the arc or cylinder they lie on. Copper is flat, as in
  KiCad's VRML export: its top and bottom faces without walls, less the
  faces resting on the body and copper inside it.
- **Models**: one mesh per distinct embedded payload, with a primitive per
  STEP colour, drawn for every footprint that uses it with
  `EXT_mesh_gpu_instancing`; the node's `extras.references` lists the
  footprints in instance order. Models are tessellated within 0.01 mm, the
  largest first, in parallel, then simplified within another 0.01 mm. Faces
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
