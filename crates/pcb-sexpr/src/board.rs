//! KiCad board file (.kicad_pcb) utilities.
//!
//! Structural predicates for identifying specific string positions in KiCad PCB files.

use crate::Sexpr;
use crate::WalkCtx;
use crate::{kicad as sexpr_kicad, Span};
use std::collections::BTreeMap;

/// Check if node is a group name: `(group "NAME" ...)`
pub fn is_group_name(ctx: &WalkCtx<'_>) -> bool {
    ctx.index_in_parent == Some(1) && ctx.parent_tag() == Some("group")
}

/// Check if node is a net name: `(net N "NAME")`
pub fn is_net_name(ctx: &WalkCtx<'_>) -> bool {
    ctx.index_in_parent == Some(2) && ctx.parent_tag() == Some("net")
}

/// Check if node is a footprint Path property: `(property "Path" "VALUE")` inside a footprint.
pub fn is_footprint_path_property(ctx: &WalkCtx<'_>) -> bool {
    if ctx.index_in_parent != Some(2) || ctx.parent_tag() != Some("property") {
        return false;
    }
    // Check property name is "Path"
    let parent_items = ctx.parent().and_then(|p| p.as_list());
    if parent_items.and_then(|items| items.get(1)?.as_str()) != Some("Path") {
        return false;
    }
    // Check grandparent is footprint
    ctx.grandparent_tag() == Some("footprint")
}

/// Check if node is a footprint's internal path (UUID): `(path "/UUID")` inside a footprint.
/// This is KiCad's internal identifier, not our custom Path property.
pub fn is_footprint_kiid_path(ctx: &WalkCtx<'_>) -> bool {
    // (path "/uuid") - string at index 1 inside a path list
    if ctx.index_in_parent != Some(1) || ctx.parent_tag() != Some("path") {
        return false;
    }
    // Check grandparent is footprint
    ctx.grandparent_tag() == Some("footprint")
}

/// Check if node is a zone net_name: `(net_name "NAME")` inside a zone.
pub fn is_zone_net_name(ctx: &WalkCtx<'_>) -> bool {
    // (net_name "NAME") - string at index 1 inside net_name list
    if ctx.index_in_parent != Some(1) || ctx.parent_tag() != Some("net_name") {
        return false;
    }
    // Check grandparent is zone
    ctx.grandparent_tag() == Some("zone")
}

#[derive(Debug, Clone, PartialEq)]
pub struct FootprintAt {
    pub x: f64,
    pub y: f64,
    pub rot: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FootprintPad {
    pub number: String,
    pub uuid: Option<String>,
    pub net_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FootprintInfo {
    /// Footprint identifier from `(footprint "<FPID>" ...)`.
    pub fpid: Option<String>,
    pub uuid: Option<String>,
    pub layer: Option<String>,
    pub at: Option<FootprintAt>,
    /// KiCad PCB footprint `(path "...")` value.
    pub path: String,
    pub sheetname: Option<String>,
    pub sheetfile: Option<String>,
    pub attrs: Vec<String>,
    /// All `(property "NAME" "VALUE" ...)` pairs inside the footprint.
    pub properties: BTreeMap<String, String>,
    pub pads: Vec<FootprintPad>,
    /// Byte span of the full `(footprint ...)` list node within the source text.
    pub span: Span,
}

/// Extract keyed footprints (those with a `(path "/...")`), ignoring unkeyed footprints.
pub fn extract_keyed_footprints(root: &Sexpr) -> Result<Vec<FootprintInfo>, String> {
    let root_list = root
        .as_list()
        .ok_or_else(|| "KiCad PCB root is not a list".to_string())?;

    let mut out: Vec<FootprintInfo> = Vec::new();

    for node in root_list.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        if items.first().and_then(Sexpr::as_sym) != Some("footprint") {
            continue;
        }

        let mut path: Option<String> = None;
        let mut uuid: Option<String> = None;
        let mut layer: Option<String> = None;
        let mut at: Option<FootprintAt> = None;
        let mut sheetname: Option<String> = None;
        let mut sheetfile: Option<String> = None;
        let mut attrs: Vec<String> = Vec::new();
        let mut pads: Vec<FootprintPad> = Vec::new();

        for child in items.iter().skip(1) {
            let Some(list) = child.as_list() else {
                continue;
            };
            match list.first().and_then(Sexpr::as_sym) {
                Some("path") => {
                    path = list.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
                }
                Some("uuid") => {
                    uuid = list.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
                }
                Some("layer") => {
                    layer = list.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
                }
                Some("at") => {
                    at = parse_at_list(list);
                }
                Some("sheetname") => {
                    sheetname = list.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
                }
                Some("sheetfile") => {
                    sheetfile = list.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
                }
                Some("attr") => {
                    attrs.extend(
                        list.iter()
                            .skip(1)
                            .filter_map(|n| n.as_sym().map(|s| s.to_string())),
                    );
                }
                Some("pad") => {
                    if let Some(pad) = parse_pad_list(list) {
                        pads.push(pad);
                    }
                }
                _ => {}
            }
        }

        let Some(path) = path else {
            continue;
        };

        let fpid = items.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
        let properties = sexpr_kicad::schematic_properties(items);

        out.push(FootprintInfo {
            fpid,
            uuid,
            layer,
            at,
            path,
            sheetname,
            sheetfile,
            attrs,
            properties,
            pads,
            span: node.span,
        });
    }

    Ok(out)
}

fn parse_at_list(list: &[Sexpr]) -> Option<FootprintAt> {
    let x = number_as_f64(list.get(1)?)?;
    let y = number_as_f64(list.get(2)?)?;
    let rot = list.get(3).and_then(number_as_f64);
    Some(FootprintAt { x, y, rot })
}

fn parse_pad_list(list: &[Sexpr]) -> Option<FootprintPad> {
    let number = list.get(1).and_then(Sexpr::as_str)?.to_string();

    let mut uuid: Option<String> = None;
    let mut net_name: Option<String> = None;

    for child in list.iter().skip(2) {
        let Some(items) = child.as_list() else {
            continue;
        };
        match items.first().and_then(Sexpr::as_sym) {
            Some("uuid") => {
                uuid = items.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
            }
            Some("net") => {
                net_name = items.get(2).and_then(Sexpr::as_str).map(|s| s.to_string());
            }
            _ => {}
        }
    }

    Some(FootprintPad {
        number,
        uuid,
        net_name,
    })
}

fn number_as_f64(node: &Sexpr) -> Option<f64> {
    node.as_float().or_else(|| node.as_int().map(|v| v as f64))
}

/// Extract a mapping from footprint reference designator to KiCad footprint `(path "...")`.
///
/// This is useful as a stable anchor for joining schematic/netlist/PCB data, since the PCB
/// uses `(path ...)` and the schematic/netlist use UUIDs that can be normalized into that path.
pub fn extract_footprint_refdes_to_kiid_path(
    root: &Sexpr,
) -> Result<BTreeMap<String, String>, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let footprints = extract_keyed_footprints(root)?;
    for fp in footprints {
        let Some(refdes) = fp.properties.get("Reference").cloned() else {
            continue;
        };
        if out.insert(refdes.clone(), fp.path).is_some() {
            return Err(format!(
                "KiCad PCB contains multiple footprints with refdes {refdes}"
            ));
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse, PatchSet};

    #[test]
    fn test_predicates() {
        let input = r#"(kicad_pcb
            (net 1 "VCC")
            (group "Power" (uuid "123"))
            (footprint "R"
                (path "/abc-123")
                (property "Path" "Power.R1")
            )
            (zone
                (net 0)
                (net_name "gnd")
            )
        )"#;

        let board = parse(input).unwrap();
        let mut found = Vec::new();

        board.walk_strings(|value, _span, ctx| {
            if is_net_name(&ctx) {
                found.push(format!("net:{value}"));
            } else if is_group_name(&ctx) {
                found.push(format!("group:{value}"));
            } else if is_footprint_path_property(&ctx) {
                found.push(format!("path_prop:{value}"));
            } else if is_footprint_kiid_path(&ctx) {
                found.push(format!("kiid:{value}"));
            } else if is_zone_net_name(&ctx) {
                found.push(format!("zone_net:{value}"));
            }
        });

        assert_eq!(
            found,
            vec![
                "net:VCC",
                "group:Power",
                "kiid:/abc-123",
                "path_prop:Power.R1",
                "zone_net:gnd"
            ]
        );
    }

    #[test]
    fn test_patch_strings() {
        let input = r#"(kicad_pcb (net 1 "OLD"))"#;
        let board = parse(input).unwrap();

        let mut patches = PatchSet::new();
        board.walk_strings(|value, span, ctx| {
            if is_net_name(&ctx) && value == "OLD" {
                patches.replace_string(span, "NEW");
            }
        });

        let mut result = Vec::new();
        patches.write_to(input, &mut result).unwrap();
        let result = String::from_utf8(result).unwrap();

        assert!(result.contains("\"NEW\""));
    }

    #[test]
    fn test_extract_footprint_refdes_to_kiid_path() {
        let input = r#"(kicad_pcb
            (footprint "R"
                (path "/abc-123")
                (property "Reference" "R1")
            )
            (footprint "C"
                (path "/def-456")
                (property "Reference" "C1")
            )
        )"#;

        let board = parse(input).unwrap();
        let map = extract_footprint_refdes_to_kiid_path(&board).unwrap();
        assert_eq!(map.get("R1").map(String::as_str), Some("/abc-123"));
        assert_eq!(map.get("C1").map(String::as_str), Some("/def-456"));
    }

    #[test]
    fn test_extract_keyed_footprints() {
        let input = r#"(kicad_pcb
            (footprint "R"
                (layer "F.Cu")
                (uuid "u1")
                (at 1 2 90)
                (property "Reference" "R1")
                (path "/abc-123")
                (pad "1" smd (net 1 "VCC") (uuid "p1"))
                (pad "2" smd (net 2 "GND"))
            )
            (footprint "C"
                (property "Reference" "C1")
            )
        )"#;

        let board = parse(input).unwrap();
        let fps = extract_keyed_footprints(&board).unwrap();
        assert_eq!(fps.len(), 1);
        let fp = &fps[0];
        assert_eq!(fp.fpid.as_deref(), Some("R"));
        assert_eq!(fp.layer.as_deref(), Some("F.Cu"));
        assert_eq!(fp.uuid.as_deref(), Some("u1"));
        assert_eq!(fp.path, "/abc-123");
        assert_eq!(
            fp.at,
            Some(FootprintAt {
                x: 1.0,
                y: 2.0,
                rot: Some(90.0)
            })
        );
        assert_eq!(
            fp.properties.get("Reference").map(String::as_str),
            Some("R1")
        );
        assert_eq!(fp.pads.len(), 2);
        assert_eq!(
            fp.pads[0],
            FootprintPad {
                number: "1".to_string(),
                uuid: Some("p1".to_string()),
                net_name: Some("VCC".to_string())
            }
        );
        assert_eq!(
            fp.pads[1],
            FootprintPad {
                number: "2".to_string(),
                uuid: None,
                net_name: Some("GND".to_string())
            }
        );
        assert!(fp.span.len() > 0);
    }
}
