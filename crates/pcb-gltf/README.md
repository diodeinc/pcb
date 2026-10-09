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
  vias, silkscreen and solder mask in the colours `pcb-step` writes to STEP.
  Arcs and round holes are chords within 0.01 mm, and curved walls carry the
  exact normal of the arc or cylinder they lie on. Copper faces the body
  hides are left out: caps resting on it, copper inside it, and via barrels
  when vias are not cut.
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
