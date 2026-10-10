//! glTF 2.0 binary container, written small and cheap to draw: one mesh for
//! the board with a primitive per layer, and one mesh per distinct model
//! with a primitive per colour, drawn once for all its footprints with
//! `EXT_mesh_gpu_instancing`. Positions are snapped to a 16-bit grid per mesh
//! and normals to 8 bits (`KHR_mesh_quantization`), and every vertex and index
//! buffer is compressed with `EXT_meshopt_compression`. A root node converts
//! the millimetre, z-up scene to glTF's metres, y up.

use std::f64::consts::FRAC_PI_2;
use std::io::Write;

use anyhow::{Result, ensure};
use glam::{DMat4, DVec3};
use meshopt_rs::index::IndexEncodingVersion;
use meshopt_rs::index::buffer::{encode_index_buffer, encode_index_buffer_bound};
use meshopt_rs::vertex::VertexEncodingVersion;
use meshopt_rs::vertex::buffer::{encode_vertex_buffer, encode_vertex_buffer_bound};
use meshopt_rs::vertex::filter::encode_filter_oct_8;
use pcb_step::scene::{LayerKind, Scene};
use rayon::prelude::*;
use serde_json::{Value, json};

use crate::mesh::Primitive;
use crate::models::Models;

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const BYTE: u32 = 5120;
const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;

const QUANTIZATION: &str = "KHR_mesh_quantization";
const MESHOPT: &str = "EXT_meshopt_compression";
const INSTANCING: &str = "EXT_mesh_gpu_instancing";

#[derive(Clone, Copy, PartialEq)]
struct Material {
    /// Linear RGBA.
    color: [f32; 4],
    metallic: f32,
    roughness: f32,
    double_sided: bool,
}

impl Material {
    fn json(&self) -> Value {
        let mut material = json!({
            "pbrMetallicRoughness": {
                "baseColorFactor": self.color,
                "metallicFactor": self.metallic,
                "roughnessFactor": self.roughness,
            },
        });
        if self.color[3] < 1.0 {
            material["alphaMode"] = json!("BLEND");
        }
        if self.double_sided {
            material["doubleSided"] = json!(true);
        }
        material
    }
}

/// Board materials: the layer colours KiCad writes to STEP, with copper
/// shown as metal.
fn layer_material(kind: LayerKind, srgb: [f64; 3], transparency: Option<f64>) -> Material {
    let (metallic, roughness) = match kind {
        LayerKind::Body => (0.0, 0.8),
        // Not metallic: without an environment map, metal renders black.
        LayerKind::Copper | LayerKind::Pads | LayerKind::Vias => (0.0, 0.4),
        LayerKind::Silkscreen { .. } => (0.0, 0.9),
        LayerKind::Soldermask { .. } => (0.0, 0.4),
    };
    let [r, g, b] = srgb.map(|c| srgb_to_linear(c as f32));
    Material {
        color: [r, g, b, 1.0 - transparency.unwrap_or(0.0) as f32],
        metallic,
        roughness,
        double_sided: false,
    }
}

/// Model materials: STEP colours are sRGB. Models may have open shells, so
/// both sides are drawn.
fn model_material(srgba: [f32; 4]) -> Material {
    let [r, g, b, a] = srgba;
    Material {
        color: [srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b), a],
        metallic: 0.0,
        roughness: 0.5,
        double_sided: true,
    }
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The 16-bit grid a mesh's positions are snapped to: a position is
/// `origin + q * step`. One step on every axis keeps the dequantization a
/// uniform scale, which instance transforms can absorb.
#[derive(Clone, Copy)]
struct Grid {
    origin: DVec3,
    step: f64,
}

impl Grid {
    fn of<'a>(primitives: impl IntoIterator<Item = &'a Primitive>) -> Self {
        let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
        for p in primitives.into_iter().flat_map(|p| &p.positions) {
            let p = DVec3::from(p.map(f64::from));
            lo = lo.min(p);
            hi = hi.max(p);
        }
        let extent = (hi - lo).max_element();
        Grid {
            origin: lo,
            step: if extent > 0.0 { extent / 65535.0 } else { 1.0 },
        }
    }

    /// Grid units to millimetres.
    fn matrix(&self) -> DMat4 {
        DMat4::from_translation(self.origin) * DMat4::from_scale(DVec3::splat(self.step))
    }
}

/// A primitive's buffers, quantized and compressed.
struct Encoded {
    vertices: usize,
    positions: Vec<u8>,
    /// Grid bounds of the positions.
    bounds: [[u16; 3]; 2],
    normals: Option<Vec<u8>>,
    indices: Vec<u8>,
    index_count: usize,
}

/// Encode `primitive`, with its normals unless it is flat: loaders shade a
/// primitive without normals flat, which is exact for planar faces.
fn encode(primitive: &Primitive, grid: Grid, flat: bool) -> Encoded {
    let positions: Vec<[u16; 4]> = primitive
        .positions
        .iter()
        .map(|p| {
            let q = ((DVec3::from(p.map(f64::from)) - grid.origin) / grid.step)
                .round()
                .clamp(DVec3::ZERO, DVec3::splat(65535.0));
            [q.x as u16, q.y as u16, q.z as u16, 0]
        })
        .collect();
    let mut bounds = [[u16::MAX; 3], [0; 3]];
    for q in &positions {
        for k in 0..3 {
            bounds[0][k] = bounds[0][k].min(q[k]);
            bounds[1][k] = bounds[1][k].max(q[k]);
        }
    }
    // Normals take the octahedral filter: two 8-bit components that the
    // decoder expands back to a unit vector, which compress better than
    // three independent ones.
    let normals = (!flat).then(|| {
        let unit: Vec<[f32; 4]> = primitive
            .normals
            .iter()
            .map(|&[x, y, z]| [x, y, z, 0.0])
            .collect();
        let mut normals = vec![[0u8; 4]; unit.len()];
        encode_filter_oct_8(normals.iter_mut(), 8, unit.iter());
        encode_vertices(&normals)
    });
    let mut indices = vec![0; encode_index_buffer_bound(primitive.indices.len(), positions.len())];
    let size = encode_index_buffer(&mut indices, &primitive.indices, IndexEncodingVersion::V1)
        .expect("buffer is at the bound");
    indices.truncate(size);
    Encoded {
        vertices: positions.len(),
        positions: encode_vertices(&positions),
        bounds,
        normals,
        indices,
        index_count: primitive.indices.len(),
    }
}

fn encode_vertices<V>(vertices: &[V]) -> Vec<u8> {
    let mut out = vec![0; encode_vertex_buffer_bound(vertices.len(), size_of::<V>())];
    let size = encode_vertex_buffer(&mut out, vertices, VertexEncodingVersion::V0)
        .expect("buffer is at the bound");
    out.truncate(size);
    out
}

#[derive(Default)]
struct Gltf {
    /// The GLB's binary chunk: compressed buffers and instance data.
    bin: Vec<u8>,
    /// Size of the buffer the compressed views decode into.
    decoded: usize,
    views: Vec<Value>,
    accessors: Vec<Value>,
    materials: Vec<Material>,
}

impl Gltf {
    fn push_bin(&mut self, bytes: &[u8]) -> usize {
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.bin.resize(self.bin.len().next_multiple_of(4), 0);
        offset
    }

    /// A view that decodes `data` into `count` elements of `stride` bytes.
    fn compressed_view(
        &mut self,
        data: &[u8],
        stride: usize,
        count: usize,
        mode: &str,
        filter: Option<&str>,
        target: u32,
    ) -> usize {
        let offset = self.push_bin(data);
        let length = stride * count;
        let mut view = json!({
            "buffer": 1,
            "byteOffset": self.decoded,
            "byteLength": length,
            "target": target,
            "extensions": { MESHOPT: {
                "buffer": 0,
                "byteOffset": offset,
                "byteLength": data.len(),
                "byteStride": stride,
                "count": count,
                "mode": mode,
            }},
        });
        if mode == "ATTRIBUTES" {
            view["byteStride"] = json!(stride);
        }
        if let Some(filter) = filter {
            view["extensions"][MESHOPT]["filter"] = json!(filter);
        }
        self.decoded = (self.decoded + length).next_multiple_of(4);
        self.views.push(view);
        self.views.len() - 1
    }

    fn accessor(&mut self, accessor: Value) -> usize {
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }

    /// Instance data, uncompressed.
    fn floats<const N: usize>(&mut self, values: &[[f32; N]], kind: &str) -> usize {
        let bytes: Vec<u8> = values
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let offset = self.push_bin(&bytes);
        self.views
            .push(json!({ "buffer": 0, "byteOffset": offset, "byteLength": bytes.len() }));
        let view = self.views.len() - 1;
        self.accessor(json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": values.len(),
            "type": kind,
        }))
    }

    fn primitive(&mut self, encoded: &Encoded, material: Material) -> Value {
        let view = self.compressed_view(
            &encoded.positions,
            8,
            encoded.vertices,
            "ATTRIBUTES",
            None,
            ARRAY_BUFFER,
        );
        let position = self.accessor(json!({
            "bufferView": view,
            "componentType": UNSIGNED_SHORT,
            "count": encoded.vertices,
            "type": "VEC3",
            "min": encoded.bounds[0],
            "max": encoded.bounds[1],
        }));
        let mut attributes = json!({ "POSITION": position });
        if let Some(normals) = &encoded.normals {
            let view = self.compressed_view(
                normals,
                4,
                encoded.vertices,
                "ATTRIBUTES",
                Some("OCTAHEDRAL"),
                ARRAY_BUFFER,
            );
            attributes["NORMAL"] = json!(self.accessor(json!({
                "bufferView": view,
                "componentType": BYTE,
                "normalized": true,
                "count": encoded.vertices,
                "type": "VEC3",
            })));
        }
        // The largest index value is reserved for primitive restart.
        let (stride, component) = if encoded.vertices <= u16::MAX as usize {
            (2, UNSIGNED_SHORT)
        } else {
            (4, UNSIGNED_INT)
        };
        let view = self.compressed_view(
            &encoded.indices,
            stride,
            encoded.index_count,
            "TRIANGLES",
            None,
            ELEMENT_ARRAY_BUFFER,
        );
        let indices = self.accessor(json!({
            "bufferView": view,
            "componentType": component,
            "count": encoded.index_count,
            "type": "SCALAR",
        }));
        let material = match self.materials.iter().position(|m| *m == material) {
            Some(i) => i,
            None => {
                self.materials.push(material);
                self.materials.len() - 1
            }
        };
        json!({
            "attributes": attributes,
            "indices": indices,
            "material": material,
        })
    }
}

fn matrix(m: DMat4) -> Value {
    json!(m.to_cols_array())
}

pub(crate) fn write(
    name: &str,
    scene: &Scene,
    layers: &[Primitive],
    models: &Models,
    sink: &mut dyn Write,
) -> Result<()> {
    let board: Vec<(&pcb_step::scene::Layer, &Primitive)> = scene
        .layers
        .iter()
        .zip(layers)
        .filter(|(_, primitive)| !primitive.is_empty())
        .collect();
    let board_grid = Grid::of(board.iter().map(|(_, p)| *p));
    let model_grids: Vec<Grid> = models
        .meshes
        .iter()
        .map(|mesh| Grid::of(mesh.primitives.iter().map(|(_, p)| p)))
        .collect();

    // Quantize and compress every primitive in parallel, in output order.
    // Every board layer but the body is flat.
    let jobs: Vec<(&Primitive, Grid, bool)> = board
        .iter()
        .map(|(layer, p)| (*p, board_grid, layer.kind != LayerKind::Body))
        .chain(
            models
                .meshes
                .iter()
                .zip(&model_grids)
                .flat_map(|(mesh, grid)| {
                    mesh.primitives.iter().map(move |(_, p)| (p, *grid, false))
                }),
        )
        .collect();
    let encoded: Vec<Encoded> = jobs
        .par_iter()
        .map(|(p, grid, flat)| encode(p, *grid, *flat))
        .collect();
    let mut encoded = encoded.iter();

    let mut gltf = Gltf::default();
    let mut meshes = Vec::new();
    let mut nodes = vec![Value::Null];
    let mut children = Vec::new();

    if !board.is_empty() {
        let primitives: Vec<Value> = board
            .iter()
            .map(|(layer, _)| {
                let material = layer_material(layer.kind, layer.color, layer.transparency);
                gltf.primitive(encoded.next().unwrap(), material)
            })
            .collect();
        meshes.push(json!({ "name": format!("{name}_PCB"), "primitives": primitives }));
        nodes.push(json!({ "name": "PCB", "mesh": 0, "matrix": matrix(board_grid.matrix()) }));
        children.push(nodes.len() - 1);
    }

    // Each model's placements, in footprint order.
    let mut instances: Vec<Vec<(&str, DMat4)>> = vec![Vec::new(); models.meshes.len()];
    for component in &scene.components {
        if let Some(mesh) = models.of_model[component.model] {
            let scale = scene.models[component.model].scale;
            let placement = component.transform
                * DMat4::from_scale(DVec3::splat(scale))
                * model_grids[mesh].matrix();
            instances[mesh].push((&component.reference, placement));
        }
    }
    for (mesh, placements) in models.meshes.iter().zip(&instances) {
        let primitives: Vec<Value> = mesh
            .primitives
            .iter()
            .map(|(color, _)| gltf.primitive(encoded.next().unwrap(), model_material(*color)))
            .collect();
        meshes.push(json!({ "name": mesh.name, "primitives": primitives }));
        // Placements are rotations with uniform scale, so each is exactly a
        // translation, rotation and scale.
        let (mut translations, mut rotations, mut scales) = (Vec::new(), Vec::new(), Vec::new());
        for (_, placement) in placements {
            let (scale, rotation, translation) = placement.to_scale_rotation_translation();
            translations.push(translation.as_vec3().to_array());
            rotations.push(rotation.normalize().as_quat().to_array());
            scales.push(scale.as_vec3().to_array());
        }
        let attributes = json!({
            "TRANSLATION": gltf.floats(&translations, "VEC3"),
            "ROTATION": gltf.floats(&rotations, "VEC4"),
            "SCALE": gltf.floats(&scales, "VEC3"),
        });
        let references: Vec<&str> = placements.iter().map(|(r, _)| *r).collect();
        nodes.push(json!({
            "name": mesh.name,
            "mesh": meshes.len() - 1,
            "extensions": { INSTANCING: { "attributes": attributes } },
            "extras": { "references": references },
        }));
        children.push(nodes.len() - 1);
    }

    if children.is_empty() {
        return Err(pcb_step::Error::NothingToExport.into());
    }

    // Millimetres to metres, then z up to y up.
    let root = DMat4::from_rotation_x(-FRAC_PI_2) * DMat4::from_scale(DVec3::splat(0.001));
    nodes[0] = json!({ "name": name, "matrix": matrix(root), "children": children });

    let extensions = [MESHOPT, QUANTIZATION, INSTANCING];
    let document = json!({
        "asset": { "version": "2.0", "generator": "pcb-gltf" },
        "extensionsUsed": extensions,
        "extensionsRequired": extensions,
        "scene": 0,
        "scenes": [{ "name": name, "nodes": [0] }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": gltf.materials.iter().map(Material::json).collect::<Vec<_>>(),
        "accessors": gltf.accessors,
        "bufferViews": gltf.views,
        "buffers": [
            { "byteLength": gltf.bin.len() },
            { "byteLength": gltf.decoded, "extensions": { MESHOPT: { "fallback": true } } },
        ],
    });
    let mut json = serde_json::to_vec(&document)?;
    json.resize(json.len().next_multiple_of(4), b' ');

    let total = 12 + 8 + json.len() + 8 + gltf.bin.len();
    ensure!(
        total <= u32::MAX as usize,
        "GLB would be {total} bytes, over the format's 4 GiB limit"
    );
    sink.write_all(b"glTF")?;
    sink.write_all(&2u32.to_le_bytes())?;
    sink.write_all(&(total as u32).to_le_bytes())?;
    sink.write_all(&(json.len() as u32).to_le_bytes())?;
    sink.write_all(b"JSON")?;
    sink.write_all(&json)?;
    sink.write_all(&(gltf.bin.len() as u32).to_le_bytes())?;
    sink.write_all(b"BIN\0")?;
    sink.write_all(&gltf.bin)?;
    Ok(())
}
