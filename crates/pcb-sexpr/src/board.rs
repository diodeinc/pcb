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

/// Transform a board-instance footprint S-expression into a standalone footprint suitable for a
/// `.kicad_mod` file.
///
/// KiCad PCB files contain board-instance footprints that include placement and connectivity
/// fields like `(at ...)`, `(path ...)`, and per-pad `(net ...)`. Standalone library footprints
/// should not include these.
///
/// This function:
/// - Keeps geometry, pad definitions, layer/attrs, and other footprint-local data.
/// - Removes board-instance fields: `at`, `path`, `sheetname`, `sheetfile`, `property`, `locked`.
/// - Removes per-pad `net` and `uuid` nodes.
/// - Ensures `(version ...)` and `(generator ...)` exist and appear at the top (and preserves
///   `(generator_version ...)` if present).
pub fn transform_board_instance_footprint_to_standalone(
    footprint_sexpr: &str,
) -> Result<String, String> {
    let root = crate::parse(footprint_sexpr).map_err(|e| format!("{e:#}"))?;
    let items = root
        .as_list()
        .ok_or_else(|| "footprint is not a list".to_string())?;
    if items.first().and_then(Sexpr::as_sym) != Some("footprint") {
        return Err("expected (footprint ...)".to_string());
    }

    let Some(raw_name) = items.get(1).and_then(Sexpr::as_str) else {
        return Err("expected (footprint \"<name>\" ...)".to_string());
    };
    let name = footprint_name_from_fpid(raw_name);

    let mut out_items: Vec<Sexpr> = Vec::new();
    out_items.push(Sexpr::symbol("footprint"));
    out_items.push(Sexpr::string(name));

    let mut version_node: Option<Sexpr> = None;
    let mut generator_node: Option<Sexpr> = None;
    let mut generator_version_node: Option<Sexpr> = None;
    let mut has_fp_text = false;

    // First pass: keep most children, filtering board-instance fields.
    let mut children: Vec<Sexpr> = Vec::new();
    for child in items.iter().skip(2) {
        let Some(list) = child.as_list() else {
            // Preserve raw atoms/comments if any (unlikely, but harmless).
            children.push(child.clone());
            continue;
        };

        let tag = list.first().and_then(Sexpr::as_sym).unwrap_or("");
        match tag {
            "at" | "path" | "sheetname" | "sheetfile" | "property" | "locked" => continue,
            "uuid" => continue,
            "version" => {
                if version_node.is_none() {
                    version_node = Some(child.clone());
                }
            }
            "generator" => {
                if generator_node.is_none() {
                    generator_node = Some(child.clone());
                }
            }
            "generator_version" => {
                if generator_version_node.is_none() {
                    generator_version_node = Some(child.clone());
                }
            }
            "fp_text" => {
                has_fp_text = true;
                children.push(child.clone());
            }
            "pad" => {
                children.push(filter_pad(child)?);
            }
            _ => children.push(child.clone()),
        }
    }

    // Canonicalize header fields: ensure version/generator exist and appear at the top.
    out_items.push(
        version_node
            .unwrap_or_else(|| Sexpr::list(vec![Sexpr::symbol("version"), Sexpr::int(20211014)])),
    );
    out_items.push(
        generator_node.unwrap_or_else(|| {
            Sexpr::list(vec![Sexpr::symbol("generator"), Sexpr::symbol("pcbnew")])
        }),
    );
    if let Some(node) = generator_version_node {
        out_items.push(node);
    }

    // Many `.kicad_mod` footprints contain fp_text reference/value/user.
    // If the instance footprint did not include them, add minimal placeholders.
    if !has_fp_text {
        let fp_texts: Vec<Sexpr> = vec![
            min_fp_text("reference", "REF**", "F.SilkS"),
            min_fp_text(
                "value",
                items.get(1).and_then(Sexpr::as_str).unwrap_or(""),
                "F.Fab",
            ),
            min_fp_text("user", "${REFERENCE}", "F.Fab"),
        ];
        out_items.extend(fp_texts);
    }

    out_items.extend(children);

    Ok(Sexpr::list(out_items).to_string())
}

fn footprint_name_from_fpid(fpid: &str) -> String {
    // `lib:fpname` -> `fpname`
    fpid.rsplit_once(':')
        .map(|(_, name)| name)
        .unwrap_or(fpid)
        .trim()
        .to_string()
}

fn filter_pad(pad_node: &Sexpr) -> Result<Sexpr, String> {
    let items = pad_node
        .as_list()
        .ok_or_else(|| "pad is not a list".to_string())?;
    if items.first().and_then(Sexpr::as_sym) != Some("pad") {
        return Err("expected (pad ...)".to_string());
    }

    let mut out: Vec<Sexpr> = Vec::new();
    out.push(Sexpr::symbol("pad"));
    // Keep pad number/type/shape atoms.
    for item in items.iter().skip(1).take(3) {
        out.push(item.clone());
    }

    for child in items.iter().skip(4) {
        let Some(list) = child.as_list() else {
            out.push(child.clone());
            continue;
        };
        match list.first().and_then(Sexpr::as_sym) {
            Some("net") => continue,
            Some("uuid") => continue,
            _ => out.push(child.clone()),
        }
    }
    Ok(Sexpr::list(out))
}

fn min_fp_text(kind: &str, value: &str, layer: &str) -> Sexpr {
    Sexpr::list(vec![
        Sexpr::symbol("fp_text"),
        Sexpr::symbol(kind),
        Sexpr::string(value),
        Sexpr::list(vec![Sexpr::symbol("at"), Sexpr::int(0), Sexpr::int(0)]),
        Sexpr::list(vec![Sexpr::symbol("layer"), Sexpr::string(layer)]),
        Sexpr::list(vec![
            Sexpr::symbol("effects"),
            Sexpr::list(vec![
                Sexpr::symbol("font"),
                Sexpr::list(vec![Sexpr::symbol("size"), Sexpr::int(1), Sexpr::int(1)]),
                Sexpr::list(vec![Sexpr::symbol("thickness"), Sexpr::float(0.15)]),
            ]),
        ]),
    ])
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
        assert!(!fp.span.is_empty());
    }

    #[test]
    fn standalone_footprint_filters_instance_fields() {
        let input = r#"
        (footprint "my-lib:R_0402_1005Metric"
            (layer "F.Cu")
            (uuid "u1")
            (at 10 20 90)
            (property "Reference" "R1")
            (property "Value" "10k")
            (path "/abc/def")
            (sheetname "Top")
            (sheetfile "top.kicad_sch")
            (attr smd)
            (fp_line (start 0 0) (end 1 1) (stroke (width 0.1) (type solid)) (layer "F.SilkS"))
            (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu") (net 1 "VCC") (uuid "p1"))
            (pad "2" smd rect (at 0 0) (size 1 1) (layers "F.Cu") (net 2 "GND"))
        )
        "#;

        let out = transform_board_instance_footprint_to_standalone(input).unwrap();
        let parsed = parse(&out).unwrap();
        let items = parsed.as_list().unwrap();
        assert_eq!(items.first().and_then(Sexpr::as_sym), Some("footprint"));
        assert_eq!(
            items.get(1).and_then(Sexpr::as_str),
            Some("R_0402_1005Metric")
        );

        // Board-instance fields removed.
        assert!(!out.contains("(path "));
        assert!(!out.contains("(sheetname "));
        assert!(!out.contains("(sheetfile "));
        assert!(!out.contains("(property "));
        assert!(!out.contains("(at 10 20 90"));

        // Pad nets/uuids removed.
        assert!(!out.contains("(net 1 \"VCC\")"));
        assert!(!out.contains("(net 2 \"GND\")"));
        assert!(!out.contains("(uuid \"p1\")"));

        // Has required header fields.
        assert!(out.contains("(version 20211014)"));
        assert!(out.contains("(generator pcbnew)"));
    }

    #[test]
    fn standalone_footprint_inserts_fp_text_after_headers() {
        let input = r#"
        (footprint "my-lib:R_0402_1005Metric"
            (layer "F.Cu")
            (generator pcbnew)
            (version 20211014)
            (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu") (net 1 "VCC"))
        )
        "#;

        let out = transform_board_instance_footprint_to_standalone(input).unwrap();
        let version_pos = out.find("(version").unwrap();
        let generator_pos = out.find("(generator").unwrap();
        let fp_text_pos = out.find("(fp_text").unwrap();

        assert!(version_pos < fp_text_pos);
        assert!(generator_pos < fp_text_pos);
    }

    #[test]
    fn standalone_footprint_canonicalizes_header_order() {
        let input = r#"
        (footprint "my-lib:R_0402_1005Metric"
            (layer "F.Cu")
            (fp_line (start 0 0) (end 1 1) (stroke (width 0.1) (type solid)) (layer "F.SilkS"))
            (version 20211014)
            (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu") (net 1 "VCC"))
            (generator pcbnew)
        )
        "#;

        let out = transform_board_instance_footprint_to_standalone(input).unwrap();
        let root = crate::parse(&out).unwrap();
        let items = root.as_list().unwrap();
        assert_eq!(items.first().and_then(Sexpr::as_sym), Some("footprint"));
        assert_eq!(
            items.get(1).and_then(Sexpr::as_str),
            Some("R_0402_1005Metric")
        );
        assert_eq!(
            items
                .get(2)
                .and_then(|s| s.as_list())
                .and_then(|l| l.first())
                .and_then(Sexpr::as_sym),
            Some("version")
        );
        assert_eq!(
            items
                .get(3)
                .and_then(|s| s.as_list())
                .and_then(|l| l.first())
                .and_then(Sexpr::as_sym),
            Some("generator")
        );
    }
}
