//! An independent reader for the GLBs the writer makes, sharing no code
//! with it: it decodes every compressed buffer and reports, per drawn thing
//! (each board layer, each model instance), surface area and bounds in
//! metres in glTF's frame (y up), with any structural defect found while
//! decoding. A malformed GLB can make it panic.

use anyhow::{Result, anyhow};
use glam::{DMat4, DQuat, DVec3};
use meshopt_rs::index::buffer::decode_index_buffer;
use meshopt_rs::vertex::buffer::decode_vertex_buffer;
use meshopt_rs::vertex::filter::decode_filter_oct_8;
use serde_json::{Value, json};

struct Glb {
    json: Value,
    bin: Vec<u8>,
}

impl Glb {
    fn read(d: &[u8]) -> Result<Self> {
        anyhow::ensure!(&d[0..4] == b"glTF", "not a GLB");
        let json_len = u32::from_le_bytes(d[12..16].try_into()?) as usize;
        let json = serde_json::from_slice(&d[20..20 + json_len])?;
        let bin = if d.len() == 20 + json_len {
            &[][..]
        } else {
            &d[20 + json_len + 8..]
        };
        Ok(Glb {
            json,
            bin: bin.to_vec(),
        })
    }

    fn view(&self, accessor: &Value) -> &Value {
        &self.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize]["extensions"]["EXT_meshopt_compression"]
    }

    fn data(&self, ext: &Value) -> &[u8] {
        let off = ext["byteOffset"].as_u64().unwrap() as usize;
        &self.bin[off..off + ext["byteLength"].as_u64().unwrap() as usize]
    }

    fn accessor(&self, index: &Value) -> &Value {
        &self.json["accessors"][index.as_u64().unwrap() as usize]
    }

    fn count(accessor: &Value) -> usize {
        accessor["count"].as_u64().unwrap() as usize
    }

    /// Decoded 4-element vertex attribute.
    fn attribute<T: Copy + Default>(&self, accessor: &Value) -> Result<Vec<[T; 4]>> {
        let mut out = vec![[T::default(); 4]; Self::count(accessor)];
        decode_vertex_buffer(&mut out, self.data(self.view(accessor)))
            .map_err(|e| anyhow!("{e:?}"))?;
        Ok(out)
    }

    /// Uncompressed floats, as instance transforms are stored.
    fn floats<const N: usize>(&self, accessor: &Value) -> Vec<[f32; N]> {
        let view = &self.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
        self.data(view)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect::<Vec<_>>()
            .as_chunks::<N>()
            .0
            .to_vec()
    }

    fn indices(&self, accessor: &Value) -> Result<Vec<u32>> {
        let mut out = vec![0u32; Self::count(accessor)];
        decode_index_buffer(&mut out, self.data(self.view(accessor)))
            .map_err(|e| anyhow!("{e:?}"))?;
        Ok(out)
    }
}

/// Area and bounds of one primitive placed by `m`, and its defects.
fn primitive(
    g: &Glb,
    p: &Value,
    m: DMat4,
    label: &str,
    defects: &mut Vec<String>,
) -> Result<Value> {
    let positions: Vec<DVec3> = g
        .attribute::<u16>(g.accessor(&p["attributes"]["POSITION"]))?
        .iter()
        .map(|q| m.transform_point3(DVec3::new(q[0] as f64, q[1] as f64, q[2] as f64)))
        .collect();
    // Flat layers carry no normals.
    let mut normals = match p["attributes"].get("NORMAL") {
        Some(normal) => g.attribute::<u8>(g.accessor(normal))?,
        None => Vec::new(),
    };
    decode_filter_oct_8(normals.iter_mut());
    let bad = normals
        .iter()
        .map(|n| DVec3::new(n[0] as i8 as f64, n[1] as i8 as f64, n[2] as i8 as f64) / 127.0)
        .filter(|n| (n.length() - 1.0).abs() > 0.05)
        .count();
    if bad > 0 {
        defects.push(format!("{label}: {bad} non-unit normals"));
    }
    let indices = g.indices(g.accessor(&p["indices"]))?;
    if indices.iter().any(|&i| i as usize >= positions.len()) {
        defects.push(format!("{label}: index out of range"));
        return Ok(Value::Null);
    }
    let (mut lo, mut hi) = (DVec3::INFINITY, DVec3::NEG_INFINITY);
    // Area of faces turned up, down and sideways, along the frame's y.
    let mut area = [0.0f64; 3];
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| positions[i as usize]);
        let cross = (b - a).cross(c - a);
        let up = cross.normalize_or_zero().y;
        area[if up > 0.999 {
            0
        } else if up < -0.999 {
            1
        } else {
            2
        }] += cross.length() * 0.5;
        for v in [a, b, c] {
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    let material = &g.json["materials"][p["material"].as_u64().unwrap() as usize];
    Ok(json!({
        "area": area.iter().sum::<f64>(),
        "areaUp": area[0],
        "areaDown": area[1],
        "min": lo.to_array(),
        "max": hi.to_array(),
        "color": material["pbrMetallicRoughness"]["baseColorFactor"],
        "extras": p["extras"],
    }))
}

/// Every drawn thing in a GLB, the scene manifest and the JSON document.
pub fn stats_of(bytes: &[u8]) -> Result<Value> {
    let g = Glb::read(bytes)?;
    let mut defects = Vec::new();
    let mut things = Vec::new();
    let nodes = g.json["nodes"].as_array().unwrap();
    let mut stack = vec![(0usize, DMat4::IDENTITY)];
    while let Some((index, parent)) = stack.pop() {
        let node = &nodes[index];
        let m = match node["matrix"].as_array() {
            Some(a) => {
                let v: Vec<f64> = a.iter().map(|x| x.as_f64().unwrap()).collect();
                parent * DMat4::from_cols_slice(&v)
            }
            None => parent,
        };
        for child in node["children"].as_array().into_iter().flatten() {
            stack.push((child.as_u64().unwrap() as usize, m));
        }
        let Some(mesh) = node["mesh"].as_u64() else {
            continue;
        };
        let prims = g.json["meshes"][mesh as usize]["primitives"]
            .as_array()
            .unwrap();
        let label = node["name"].to_string();
        let instancing = &node["extensions"]["EXT_mesh_gpu_instancing"]["attributes"];
        if instancing.is_null() {
            let list = prims
                .iter()
                .map(|p| primitive(&g, p, m, &label, &mut defects))
                .collect::<Result<Vec<_>>>()?;
            things.push(json!({ "kind": "board", "primitives": list }));
            continue;
        }
        let t = g.floats::<3>(g.accessor(&instancing["TRANSLATION"]));
        let r = g.floats::<4>(g.accessor(&instancing["ROTATION"]));
        let s = g.floats::<3>(g.accessor(&instancing["SCALE"]));
        let references = node["extras"]["references"].as_array().unwrap();
        if references.len() != t.len() {
            defects.push(format!(
                "{label}: {} references for {} instances",
                references.len(),
                t.len()
            ));
        }
        for k in 0..t.len() {
            let instance = m * DMat4::from_scale_rotation_translation(
                DVec3::from(s[k].map(f64::from)),
                DQuat::from_array(r[k].map(f64::from)),
                DVec3::from(t[k].map(f64::from)),
            );
            let list = prims
                .iter()
                .map(|p| primitive(&g, p, instance, &label, &mut defects))
                .collect::<Result<Vec<_>>>()?;
            things.push(json!({
                "kind": "model",
                "name": node["name"],
                "reference": references.get(k),
                "primitives": list,
            }));
        }
    }
    Ok(json!({
        "defects": defects,
        "things": things,
        "pcb": nodes[0]["extras"]["pcb"],
        "json": g.json,
    }))
}
