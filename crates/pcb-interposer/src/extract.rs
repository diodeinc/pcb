//! Contact extract from KiCad `.kicad_pcb`, Path-keyed IPC, and `.zen` ict maps.

use std::collections::BTreeMap;
use std::path::Path;

use pcb_sexpr::board::{extract_keyed_footprints, footprint_name_from_fpid};
use pcb_sexpr::parse;

use crate::types::{BoardId, Contact, ContactId, Ict, InterposerError};

const TP_NAME: &str = "TestPoint_Pad_D1.0mm";

/// v1 contacts are bottom-side only (KiCad `B.Cu` / IPC `B.Cu` / `Bottom`).
pub fn is_bottom_copper(layer: &str) -> bool {
    let l = layer.trim();
    let lower = l.to_ascii_lowercase();
    lower == "b.cu" || lower == "bottom" || lower.starts_with("b.")
}

pub fn is_v1_testpoint(package: &str) -> bool {
    let name = package.rsplit(':').next().unwrap_or(package);
    let name = name
        .strip_suffix("_N")
        .or_else(|| name.strip_suffix("_1"))
        .unwrap_or(name);
    name == TP_NAME || name.starts_with("TestPoint_Pad_D1.0mm")
}

pub fn parse_zen_ict_map(src: &str) -> BTreeMap<String, Ict> {
    let mut out = BTreeMap::new();
    for raw in src.split("TestPoint(").skip(1) {
        let body = raw.split(')').next().unwrap_or(raw);
        let name = kv(body, "name");
        let ict = kv(body, "ict").and_then(|s| Ict::parse_name(&s));
        if let (Some(name), Some(ict)) = (name, ict) {
            out.insert(name, ict);
        }
    }
    out
}

fn kv(body: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=");
    let rest = body.split(&pat).nth(1)?;
    let rest = rest.trim_start();
    if let Some(s) = rest.strip_prefix('"') {
        return Some(s.split('"').next().unwrap_or("").to_string());
    }
    Some(
        rest.split([',', ' ', ')'])
            .next()
            .unwrap_or("")
            .trim()
            .to_string(),
    )
}

pub fn extract_kicad_src(
    src: &str,
    board: BoardId,
    zen_ict: &BTreeMap<String, Ict>,
) -> Result<Vec<Contact>, InterposerError> {
    let root = parse(src).map_err(|e| InterposerError::Other(e.to_string()))?;
    let fps = extract_keyed_footprints(&root).map_err(InterposerError::Other)?;
    let mut out = Vec::new();
    let mut next = 0u32;
    for fp in fps {
        let package = fp
            .fpid
            .as_deref()
            .map(footprint_name_from_fpid)
            .unwrap_or_default();
        if !is_v1_testpoint(&package) {
            continue;
        }
        let path = fp
            .properties
            .get("Path")
            .cloned()
            .unwrap_or_else(|| fp.path.clone());
        let name_hint = path
            .split('.')
            .next()
            .unwrap_or(&path)
            .trim_start_matches('/')
            .to_string();
        let ict = fp
            .properties
            .get("Ict")
            .and_then(|s| Ict::parse_name(s))
            .or_else(|| zen_ict.get(&name_hint).copied())
            .or_else(|| {
                fp.properties
                    .get("Reference")
                    .and_then(|r| zen_ict.get(r).copied())
            });
        let Some(ict) = ict else {
            continue;
        };
        if ict.kind().is_none() {
            continue;
        }
        let Some(at) = fp.at else {
            continue;
        };
        let layer = fp.layer.unwrap_or_default();
        if !is_bottom_copper(&layer) {
            continue;
        }
        let side = "bottom";
        out.push(Contact {
            id: ContactId(next),
            board,
            xy: [at.x, at.y],
            ict,
            path,
            package,
            side: side.to_string(),
        });
        next += 1;
    }
    Ok(out)
}

pub fn extract_kicad_path(
    path: &Path,
    board: BoardId,
    zen_ict: &BTreeMap<String, Ict>,
) -> Result<Vec<Contact>, InterposerError> {
    let src = std::fs::read_to_string(path).map_err(|e| InterposerError::Other(e.to_string()))?;
    extract_kicad_src(&src, board, zen_ict)
}

pub fn extract_ipc_xml(xml: &str, board: BoardId) -> Result<Vec<Contact>, InterposerError> {
    let doc = ipc2581::Ipc2581::parse(xml).map_err(|e| InterposerError::Other(e.to_string()))?;
    let Some(bom) = doc.bom() else {
        return Ok(Vec::new());
    };

    let mut ict_by_part: BTreeMap<String, Ict> = BTreeMap::new();
    let mut refs: Vec<(String, String, String, String)> = Vec::new(); // part, package, layer, refdes
    for item in &bom.items {
        let part = doc.resolve(item.oem_design_number_ref).to_string();
        let ict = item.characteristics.as_ref().and_then(|ch| {
            ch.textuals.iter().find_map(|t| {
                let name = t.name.map(|s| doc.resolve(s))?;
                if !name.eq_ignore_ascii_case("Ict") {
                    return None;
                }
                Ict::parse_name(t.value.map(|s| doc.resolve(s)).unwrap_or(""))
            })
        });
        if let Some(ict) = ict {
            ict_by_part.insert(part.clone(), ict);
        }
        for rd in &item.ref_des_list {
            refs.push((
                part.clone(),
                doc.resolve(rd.package_ref).to_string(),
                doc.resolve(rd.layer_ref).to_string(),
                doc.resolve(rd.name).to_string(),
            ));
        }
    }

    let mut loc_by_part: BTreeMap<String, [f64; 2]> = BTreeMap::new();
    if let Some(ecad) = doc.ecad() {
        for step in &ecad.cad_data.steps {
            for comp in &step.components {
                let part = doc.resolve(comp.part).to_string();
                loc_by_part.insert(part, [comp.location.x, comp.location.y]);
            }
        }
    }

    let mut out = Vec::new();
    let mut next = 0u32;
    for (part, package, layer, _refdes) in refs {
        if !is_v1_testpoint(&package) {
            continue;
        }
        let Some(ict) = ict_by_part.get(&part).copied() else {
            continue;
        };
        if ict.kind().is_none() {
            continue;
        }
        if !is_bottom_copper(&layer) {
            continue;
        }
        let xy = loc_by_part.get(&part).copied().unwrap_or([0.0, 0.0]);
        let side = "bottom";
        out.push(Contact {
            id: ContactId(next),
            board,
            xy,
            ict,
            path: part,
            package,
            side: side.to_string(),
        });
        next += 1;
    }
    Ok(out)
}

pub fn board_bbox_from_kicad(src: &str) -> Option<([f64; 2], [f64; 2])> {
    let mut min = [f64::INFINITY, f64::INFINITY];
    let mut max = [f64::NEG_INFINITY, f64::NEG_INFINITY];
    let mut found = false;
    for (idx, _) in src.match_indices("(layer \"Edge.Cuts\")") {
        let window = &src[idx.saturating_sub(240)..idx];
        for tag in ["(start ", "(end ", "(xy "] {
            for chunk in window.split(tag).skip(1) {
                let mut nums = chunk.split_whitespace();
                let Some(x) = nums.next().and_then(|s| s.parse::<f64>().ok()) else {
                    continue;
                };
                let Some(y) = nums
                    .next()
                    .and_then(|s| s.trim_end_matches(')').parse::<f64>().ok())
                else {
                    continue;
                };
                min[0] = min[0].min(x);
                min[1] = min[1].min(y);
                max[0] = max[0].max(x);
                max[1] = max[1].max(y);
                found = true;
            }
        }
    }
    found.then_some((min, max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::BoardId;

    const SNIP: &str = r#"
(kicad_pcb
  (footprint "stdlib_kicad-footprints_TestPoint:TestPoint_Pad_D1.0mm"
    (layer "B.Cu")
    (at 10 20)
    (path "/abc")
    (property "Path" "TP_GND.TP")
    (property "Ict" "gnd")
    (pad "1" smd circle (at 0 0) (size 1 1) (layers "B.Cu"))
  )
  (footprint "connectors_TagConnect:TC2030-NL_SWD"
    (layer "B.Cu")
    (at 1 1)
    (property "Path" "TC1")
    (property "Ict" "swd")
    (pad "1" smd circle (at 0 0) (size 1 1) (layers "B.Cu"))
  )
)
"#;

    #[test]
    fn front_side_ict_testpoint_is_skipped() {
        let src = r#"
(kicad_pcb
  (footprint "stdlib_kicad-footprints_TestPoint:TestPoint_Pad_D1.0mm"
    (layer "F.Cu")
    (at 10 20)
    (path "/front")
    (property "Path" "TP_FRONT.TP")
    (property "Ict" "gnd")
    (pad "1" smd circle (at 0 0) (size 1 1) (layers "F.Cu"))
  )
  (footprint "stdlib_kicad-footprints_TestPoint:TestPoint_Pad_D1.0mm"
    (layer "B.Cu")
    (at 12 22)
    (path "/back")
    (property "Path" "TP_BACK.TP")
    (property "Ict" "vtarget")
    (pad "1" smd circle (at 0 0) (size 1 1) (layers "B.Cu"))
  )
)
"#;
        let cs = extract_kicad_src(src, BoardId(0), &BTreeMap::new()).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].ict, Ict::Vtarget);
        assert_eq!(cs[0].side, "bottom");
        assert!(!cs.iter().any(|c| c.path.contains("FRONT")));
    }

    #[test]
    fn finds_ict_testpoint_skips_tagconnect() {
        let cs = extract_kicad_src(SNIP, BoardId(0), &BTreeMap::new()).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].ict, Ict::Gnd);
        assert_eq!(cs[0].xy, [10.0, 20.0]);
        assert_eq!(cs[0].side, "bottom");
        assert!(cs[0].package.contains("TestPoint_Pad_D1.0mm"));
    }

    #[test]
    fn ipc_path_keyed_ict() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
  <Bom name="B">
    <BomHeader assembly="A" revision="1"/>
    <BomItem OEMDesignNumberRef="TP_GND.TP" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP1" packageRef="TestPoint_Pad_D1.0mm" populate="true" layerRef="B.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="KICAD" textualCharacteristicName="Ict" textualCharacteristicValue="gnd"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TC1" quantity="1" pinCount="6" category="ELECTRICAL">
      <RefDes name="TC1" packageRef="TC2030-NL_SWD" populate="true" layerRef="B.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="KICAD" textualCharacteristicName="Ict" textualCharacteristicValue="swd"/>
      </Characteristics>
    </BomItem>
  </Bom>
</IPC-2581>"#;
        let cs = extract_ipc_xml(xml, BoardId(0)).unwrap();
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].ict, Ict::Gnd);
        assert_eq!(cs[0].path, "TP_GND.TP");
    }
}
