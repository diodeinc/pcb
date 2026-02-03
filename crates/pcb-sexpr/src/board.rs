//! KiCad board file (.kicad_pcb) utilities.
//!
//! Structural predicates for identifying specific string positions in KiCad PCB files.

use crate::Sexpr;
use crate::WalkCtx;
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

/// Extract a mapping from footprint reference designator to KiCad footprint `(path "...")`.
///
/// This is useful as a stable anchor for joining schematic/netlist/PCB data, since the PCB
/// uses `(path ...)` and the schematic/netlist use UUIDs that can be normalized into that path.
pub fn extract_footprint_refdes_to_kiid_path(
    root: &Sexpr,
) -> Result<BTreeMap<String, String>, String> {
    let root_list = root
        .as_list()
        .ok_or_else(|| "KiCad PCB root is not a list".to_string())?;

    let mut out: BTreeMap<String, String> = BTreeMap::new();

    for node in root_list.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        if items.first().and_then(Sexpr::as_sym) != Some("footprint") {
            continue;
        }

        let mut refdes: Option<String> = None;
        let mut path: Option<String> = None;

        for child in items.iter().skip(1) {
            let Some(list) = child.as_list() else {
                continue;
            };
            match list.first().and_then(Sexpr::as_sym) {
                Some("path") => {
                    path = list.get(1).and_then(Sexpr::as_str).map(|s| s.to_string());
                }
                Some("property") => {
                    let name = list.get(1).and_then(Sexpr::as_str);
                    if name != Some("Reference") {
                        continue;
                    }
                    refdes = list.get(2).and_then(Sexpr::as_str).map(|s| s.to_string());
                }
                _ => {}
            }
        }

        let (Some(refdes), Some(path)) = (refdes, path) else {
            continue;
        };

        if out.insert(refdes.clone(), path).is_some() {
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
}
