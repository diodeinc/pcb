//! glTF 2.0 binary container: one mesh for the board with a primitive per
//! layer, one mesh per distinct model with a primitive per colour, and a
//! node per component placing its model's mesh. A root node converts the
//! millimetre, z-up scene to glTF's metres, y up.

use std::f64::consts::FRAC_PI_2;
use std::io::Write;

use anyhow::{Result, ensure};
use glam::{DMat4, DVec3};
use pcb_step::scene::{LayerKind, Scene};
use serde_json::{Value, json};

use crate::mesh::Primitive;
use crate::models::Models;

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_SHORT: u32 = 5123;
const UNSIGNED_INT: u32 = 5125;

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
        LayerKind::Copper | LayerKind::Pads | LayerKind::Vias => (1.0, 0.35),
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

#[derive(Default)]
struct Gltf {
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    materials: Vec<Material>,
}

impl Gltf {
    fn view(&mut self, bytes: Vec<u8>, target: u32) -> usize {
        self.views.push(json!({
            "buffer": 0,
            "byteOffset": self.bin.len(),
            "byteLength": bytes.len(),
            "target": target,
        }));
        self.bin.extend(bytes);
        // Every component type here is four bytes or two bytes in pairs
        // of triangles' worth; keep each view four-byte aligned.
        self.bin.resize(self.bin.len().next_multiple_of(4), 0);
        self.views.len() - 1
    }

    fn accessor(&mut self, accessor: Value) -> usize {
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }

    fn vec3s(&mut self, values: &[[f32; 3]], bounds: bool) -> usize {
        let bytes = values
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let view = self.view(bytes, ARRAY_BUFFER);
        let mut accessor = json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": values.len(),
            "type": "VEC3",
        });
        if bounds {
            let mut min = [f32::INFINITY; 3];
            let mut max = [f32::NEG_INFINITY; 3];
            for v in values {
                for k in 0..3 {
                    min[k] = min[k].min(v[k]);
                    max[k] = max[k].max(v[k]);
                }
            }
            accessor["min"] = json!(min);
            accessor["max"] = json!(max);
        }
        self.accessor(accessor)
    }

    fn primitive(&mut self, primitive: &Primitive, material: Material) -> Value {
        let position = self.vec3s(&primitive.positions, true);
        let normal = self.vec3s(&primitive.normals, false);
        // The largest index value is reserved for primitive restart.
        let (bytes, component): (Vec<u8>, _) = if primitive.positions.len() <= u16::MAX as usize {
            let bytes = primitive
                .indices
                .iter()
                .flat_map(|&i| (i as u16).to_le_bytes())
                .collect();
            (bytes, UNSIGNED_SHORT)
        } else {
            let bytes = primitive
                .indices
                .iter()
                .flat_map(|i| i.to_le_bytes())
                .collect();
            (bytes, UNSIGNED_INT)
        };
        let view = self.view(bytes, ELEMENT_ARRAY_BUFFER);
        let indices = self.accessor(json!({
            "bufferView": view,
            "componentType": component,
            "count": primitive.indices.len(),
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
            "attributes": { "POSITION": position, "NORMAL": normal },
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
    let mut gltf = Gltf::default();
    let mut meshes = Vec::new();
    let mut children = Vec::new();
    let mut nodes = vec![Value::Null];

    let board: Vec<Value> = scene
        .layers
        .iter()
        .zip(layers)
        .filter(|(_, primitive)| !primitive.is_empty())
        .map(|(layer, primitive)| {
            let material = layer_material(layer.kind, layer.color, layer.transparency);
            gltf.primitive(primitive, material)
        })
        .collect();
    if !board.is_empty() {
        meshes.push(json!({ "name": format!("{name}_PCB"), "primitives": board }));
        nodes.push(json!({ "name": "PCB", "mesh": 0 }));
        children.push(nodes.len() - 1);
    }

    let first_model = meshes.len();
    for mesh in &models.meshes {
        let primitives: Vec<Value> = mesh
            .primitives
            .iter()
            .map(|(color, primitive)| gltf.primitive(primitive, model_material(*color)))
            .collect();
        meshes.push(json!({ "name": mesh.name, "primitives": primitives }));
    }
    for component in &scene.components {
        let Some(mesh) = models.of_model[component.model] else {
            continue;
        };
        let scale = scene.models[component.model].scale;
        nodes.push(json!({
            "name": component.reference,
            "mesh": first_model + mesh,
            "matrix": matrix(component.transform * DMat4::from_scale(DVec3::splat(scale))),
        }));
        children.push(nodes.len() - 1);
    }

    // Millimetres to metres, then z up to y up.
    let root = DMat4::from_rotation_x(-FRAC_PI_2) * DMat4::from_scale(DVec3::splat(0.001));
    nodes[0] = json!({ "name": name, "matrix": matrix(root), "children": children });

    let mut document = json!({
        "asset": { "version": "2.0", "generator": "pcb-gltf" },
        "scene": 0,
        "scenes": [{ "name": name, "nodes": [0] }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": gltf.materials.iter().map(Material::json).collect::<Vec<_>>(),
        "accessors": gltf.accessors,
        "bufferViews": gltf.views,
    });
    if !gltf.bin.is_empty() {
        document["buffers"] = json!([{ "byteLength": gltf.bin.len() }]);
    }
    // glTF forbids empty top-level arrays.
    if let Some(document) = document.as_object_mut() {
        document.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    }
    let mut json = serde_json::to_vec(&document)?;
    json.resize(json.len().next_multiple_of(4), b' ');

    let bin_chunk = if gltf.bin.is_empty() {
        0
    } else {
        8 + gltf.bin.len()
    };
    let total = 12 + 8 + json.len() + bin_chunk;
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
    if !gltf.bin.is_empty() {
        sink.write_all(&(gltf.bin.len() as u32).to_le_bytes())?;
        sink.write_all(b"BIN\0")?;
        sink.write_all(&gltf.bin)?;
    }
    Ok(())
}
