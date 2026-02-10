//! KiCad board file (.kicad_pcb) utilities.
//!
//! Structural predicates for identifying specific string positions in KiCad PCB files.

use crate::number_as_f64;
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

/// Transform a board-instance footprint from `.kicad_pcb` into a standalone `.kicad_mod` footprint.
///
/// KiCad PCB files embed *placed instances* of footprints. Import cannot assume the original
/// library `.kicad_mod` is available, so we de-instance the embedded S-expression:
///
/// - Drop instance-only fields: root `(at ...)`, `(path ...)`, `(sheetname ...)`, `(sheetfile ...)`,
///   root `(uuid ...)`, and per-pad `(net ...)` / `(uuid ...)`.
/// - Normalize back-side instances to canonical front-side:
///   - Swap `B.* <-> F.*` layers.
///   - Remove `mirror` from `(justify ...)`.
/// - Normalize geometry:
///   - Local points are mirrored when on back side (flip Y).
///   - Footprint-embedded `zone` polygon points are stored in absolute board coords; convert them
///     back to footprint-local via inverse pose.
///   - Pad `(at ... ANGLE)` angles are serialized as board-absolute; convert back to pad-local.
///
/// See `docs/specs/kicad-import.md` for the full math model.
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

    let pose = FootprintInstancePose::from_board_instance_footprint(items);

    let Some(raw_name) = items.get(1).and_then(Sexpr::as_str) else {
        return Err("expected (footprint \"<name>\" ...)".to_string());
    };
    let name = footprint_name_from_fpid(raw_name);

    let mut out_items: Vec<Sexpr> = Vec::new();
    out_items.push(Sexpr::symbol("footprint"));
    out_items.push(Sexpr::string(name.clone()));

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
                children.push(deinstance_node(child, &pose, DeinstanceCtx::default())?);
            }
            _ => children.push(deinstance_node(child, &pose, DeinstanceCtx::default())?),
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
            min_fp_text("value", &name, "F.Fab"),
            min_fp_text("user", "${REFERENCE}", "F.Fab"),
        ];
        out_items.extend(fp_texts);
    }

    out_items.extend(children);

    Ok(Sexpr::list(out_items).to_string())
}

/// Convert a KiCad footprint identifier like `lib:fpname` to the footprint name `fpname`.
pub fn footprint_name_from_fpid(fpid: &str) -> String {
    // `lib:fpname` -> `fpname`
    fpid.rsplit_once(':')
        .map(|(_, name)| name)
        .unwrap_or(fpid)
        .trim()
        .to_string()
}

#[derive(Debug, Clone, Copy)]
struct FootprintInstancePose {
    tx: f64,
    ty: f64,
    rot_deg: f64,
    is_back: bool,
}

impl FootprintInstancePose {
    fn from_board_instance_footprint(items: &[Sexpr]) -> Self {
        let mut tx = 0.0;
        let mut ty = 0.0;
        let mut rot_deg = 0.0;
        let mut is_back = false;

        for child in items.iter().skip(2) {
            let Some(list) = child.as_list() else {
                continue;
            };

            match list.first().and_then(Sexpr::as_sym) {
                Some("at") => {
                    if let Some(at) = parse_at_list(list) {
                        tx = at.x;
                        ty = at.y;
                        rot_deg = at.rot.unwrap_or(0.0);
                    }
                }
                Some("layer") => {
                    if let Some(layer) = list.get(1).and_then(Sexpr::as_str) {
                        is_back = layer.starts_with("B.");
                    }
                }
                _ => {}
            }
        }

        Self {
            tx,
            ty,
            rot_deg,
            is_back,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct DeinstanceCtx {
    coord_space: CoordSpace,
    angle_semantics: AngleSemantics,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum CoordSpace {
    #[default]
    Local,
    ZoneAbs,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum AngleSemantics {
    #[default]
    None,
    PadAbs,
    TextAbs,
}

fn deinstance_node(
    node: &Sexpr,
    pose: &FootprintInstancePose,
    ctx: DeinstanceCtx,
) -> Result<Sexpr, String> {
    let Some(items) = node.as_list() else {
        return Ok(node.clone());
    };
    if items.is_empty() {
        return Ok(node.clone());
    }

    let tag = items.first().and_then(Sexpr::as_sym).unwrap_or("");

    match tag {
        "pad" => return deinstance_pad(node, pose),
        "fp_text" => {
            return deinstance_generic_list(
                node,
                pose,
                DeinstanceCtx {
                    angle_semantics: AngleSemantics::TextAbs,
                    ..ctx
                },
            )
        }
        "zone" => {
            return deinstance_generic_list(
                node,
                pose,
                DeinstanceCtx {
                    coord_space: CoordSpace::ZoneAbs,
                    angle_semantics: AngleSemantics::None,
                },
            )
        }
        "at" => return deinstance_at(node, pose, ctx.angle_semantics),
        "start" | "end" | "center" | "mid" => return deinstance_xy_tag(tag, node, pose, ctx),
        "xy" => return deinstance_xy_tag("xy", node, pose, ctx),
        "layer" => return deinstance_layer(node, pose),
        "layers" => return deinstance_layers(node, pose),
        "justify" => return deinstance_justify(node, pose),
        _ => {}
    }

    deinstance_generic_list(node, pose, ctx)
}

fn deinstance_generic_list(
    node: &Sexpr,
    pose: &FootprintInstancePose,
    ctx: DeinstanceCtx,
) -> Result<Sexpr, String> {
    let items = node
        .as_list()
        .ok_or_else(|| "list is not a list".to_string())?;

    let mut out: Vec<Sexpr> = Vec::with_capacity(items.len());
    out.push(items[0].clone());
    for child in items.iter().skip(1) {
        out.push(deinstance_node(child, pose, ctx)?);
    }
    Ok(Sexpr::list(out))
}

fn deinstance_pad(node: &Sexpr, pose: &FootprintInstancePose) -> Result<Sexpr, String> {
    let items = node
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

    let ctx = DeinstanceCtx {
        angle_semantics: AngleSemantics::PadAbs,
        coord_space: CoordSpace::Local,
    };

    for child in items.iter().skip(4) {
        let Some(list) = child.as_list() else {
            out.push(child.clone());
            continue;
        };
        match list.first().and_then(Sexpr::as_sym) {
            Some("net") => continue,
            Some("uuid") => continue,
            _ => out.push(deinstance_node(child, pose, ctx)?),
        }
    }

    Ok(Sexpr::list(out))
}

fn deinstance_at(
    node: &Sexpr,
    pose: &FootprintInstancePose,
    semantics: AngleSemantics,
) -> Result<Sexpr, String> {
    let items = node
        .as_list()
        .ok_or_else(|| "at is not a list".to_string())?;
    if items.first().and_then(Sexpr::as_sym) != Some("at") {
        return Err("expected (at ...)".to_string());
    }
    if items.len() < 3 {
        return Ok(node.clone());
    }

    let Some(x) = items.get(1).and_then(number_as_f64) else {
        return Ok(node.clone());
    };
    let Some(y) = items.get(2).and_then(number_as_f64) else {
        return Ok(node.clone());
    };

    let out_y = if pose.is_back { -y } else { y };

    let mut out: Vec<Sexpr> = Vec::with_capacity(items.len());
    out.push(Sexpr::symbol("at"));
    out.push(Sexpr::float(x));
    out.push(Sexpr::float(out_y));

    let abs_angle = items.get(3).and_then(number_as_f64).unwrap_or(0.0);

    let local_angle = match semantics {
        AngleSemantics::None => None,
        AngleSemantics::PadAbs => Some(if pose.is_back {
            pose.rot_deg - abs_angle
        } else {
            abs_angle - pose.rot_deg
        }),
        AngleSemantics::TextAbs => Some(if pose.is_back {
            pose.rot_deg + 180.0 - abs_angle
        } else {
            abs_angle - pose.rot_deg
        }),
    };

    if let Some(angle) = local_angle {
        let angle = normalize_deg(angle);
        if angle != 0.0 {
            out.push(Sexpr::float(angle));
        }
    } else if items.len() >= 4 {
        // Preserve original angle for nodes we don't reinterpret.
        out.push(items[3].clone());
    }

    // Preserve any trailing items.
    out.extend(items.iter().skip(4).cloned());

    Ok(Sexpr::list(out))
}

fn deinstance_xy_tag(
    tag: &str,
    node: &Sexpr,
    pose: &FootprintInstancePose,
    ctx: DeinstanceCtx,
) -> Result<Sexpr, String> {
    let items = node
        .as_list()
        .ok_or_else(|| format!("{tag} is not a list"))?;
    if items.first().and_then(Sexpr::as_sym) != Some(tag) {
        return Err(format!("expected ({tag} ...)"));
    }
    if items.len() < 3 {
        return Ok(node.clone());
    }

    let Some(x) = items.get(1).and_then(number_as_f64) else {
        return Ok(node.clone());
    };
    let Some(y) = items.get(2).and_then(number_as_f64) else {
        return Ok(node.clone());
    };

    let (out_x, out_y) = match ctx.coord_space {
        CoordSpace::Local => (x, if pose.is_back { -y } else { y }),
        CoordSpace::ZoneAbs => deinstance_point_from_zone_abs(x, y, pose),
    };

    let mut out: Vec<Sexpr> = Vec::with_capacity(items.len());
    out.push(Sexpr::symbol(tag));
    out.push(Sexpr::float(out_x));
    out.push(Sexpr::float(out_y));
    out.extend(items.iter().skip(3).cloned());
    Ok(Sexpr::list(out))
}

fn deinstance_point_from_zone_abs(x: f64, y: f64, pose: &FootprintInstancePose) -> (f64, f64) {
    // Inverse of: p_file = t + R(theta) * (M(p_local) if is_back else p_local)
    let mut px = x - pose.tx;
    let mut py = y - pose.ty;

    let theta = (-pose.rot_deg).to_radians();
    let (s, c) = theta.sin_cos();
    let rx = c * px - s * py;
    let ry = s * px + c * py;
    px = rx;
    py = ry;

    if pose.is_back {
        py = -py;
    }

    (px, py)
}

fn normalize_deg(mut deg: f64) -> f64 {
    deg %= 360.0;
    if deg <= -180.0 {
        deg += 360.0;
    } else if deg > 180.0 {
        deg -= 360.0;
    }

    // Prefer KiCad's canonical 180 instead of -180.
    if (deg + 180.0).abs() < 1e-9 {
        deg = 180.0;
    }

    if deg.abs() < 1e-9 {
        0.0
    } else {
        deg
    }
}

fn deinstance_layer(node: &Sexpr, pose: &FootprintInstancePose) -> Result<Sexpr, String> {
    let items = node
        .as_list()
        .ok_or_else(|| "layer is not a list".to_string())?;
    if items.first().and_then(Sexpr::as_sym) != Some("layer") {
        return Err("expected (layer ...)".to_string());
    }

    if !pose.is_back {
        return Ok(node.clone());
    }

    let Some(layer) = items.get(1).and_then(Sexpr::as_str) else {
        return Ok(node.clone());
    };

    let swapped = swap_layer_side(layer);

    let mut out: Vec<Sexpr> = Vec::with_capacity(items.len());
    out.push(Sexpr::symbol("layer"));
    out.push(Sexpr::string(swapped));
    out.extend(items.iter().skip(2).cloned());
    Ok(Sexpr::list(out))
}

fn deinstance_layers(node: &Sexpr, pose: &FootprintInstancePose) -> Result<Sexpr, String> {
    let items = node
        .as_list()
        .ok_or_else(|| "layers is not a list".to_string())?;
    if items.first().and_then(Sexpr::as_sym) != Some("layers") {
        return Err("expected (layers ...)".to_string());
    }

    if !pose.is_back {
        return Ok(node.clone());
    }

    let mut out: Vec<Sexpr> = Vec::with_capacity(items.len());
    out.push(Sexpr::symbol("layers"));
    for item in items.iter().skip(1) {
        if let Some(layer) = item.as_str() {
            out.push(Sexpr::string(swap_layer_side(layer)));
        } else {
            out.push(item.clone());
        }
    }
    Ok(Sexpr::list(out))
}

fn swap_layer_side(layer: &str) -> String {
    if let Some(rest) = layer.strip_prefix("B.") {
        format!("F.{rest}")
    } else if let Some(rest) = layer.strip_prefix("F.") {
        format!("B.{rest}")
    } else {
        layer.to_string()
    }
}

fn deinstance_justify(node: &Sexpr, pose: &FootprintInstancePose) -> Result<Sexpr, String> {
    let items = node
        .as_list()
        .ok_or_else(|| "justify is not a list".to_string())?;
    if items.first().and_then(Sexpr::as_sym) != Some("justify") {
        return Err("expected (justify ...)".to_string());
    }

    if !pose.is_back {
        return Ok(node.clone());
    }

    let mut out: Vec<Sexpr> = Vec::with_capacity(items.len());
    out.push(Sexpr::symbol("justify"));
    for item in items.iter().skip(1) {
        if item.as_sym() == Some("mirror") {
            continue;
        }
        out.push(item.clone());
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
    let x = crate::number_as_f64(list.get(1)?)?;
    let y = crate::number_as_f64(list.get(2)?)?;
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
            (at 10 20)
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
        assert!(!out.contains("(at 10 20"));

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

    #[test]
    fn standalone_footprint_deinstances_front_pad_angle() {
        // Board instance: footprint at 90 deg, pad angle serialized as board-absolute.
        let input = r#"
        (footprint "lib:TEST"
            (layer "F.Cu")
            (at 10 20 90)
            (pad "1" smd rect (at 0 0 90) (size 1 1) (layers "F.Cu"))
        )
        "#;

        let out = transform_board_instance_footprint_to_standalone(input).unwrap();
        let parsed = parse(&out).unwrap();

        let mut pad_at: Option<(usize, f64, f64, Option<f64>)> = None;
        parsed.walk(|node, _ctx| {
            if pad_at.is_some() {
                return;
            }
            let Some(items) = node.as_list() else {
                return;
            };
            if items.first().and_then(Sexpr::as_sym) != Some("pad") {
                return;
            }
            if items.get(1).and_then(Sexpr::as_str) != Some("1") {
                return;
            }
            let Some(at) = crate::find_child_list(items, "at") else {
                return;
            };
            let len = at.len();
            let x = at.get(1).and_then(number_as_f64).unwrap_or(f64::NAN);
            let y = at.get(2).and_then(number_as_f64).unwrap_or(f64::NAN);
            let a = at.get(3).and_then(number_as_f64);
            pad_at = Some((len, x, y, a));
        });

        let (len, x, y, a) = pad_at.expect("missing pad at");
        // Expect de-instanced to pad-local 0 deg, so no angle field.
        assert_eq!(len, 3);
        assert!((x - 0.0).abs() < 1e-9);
        assert!((y - 0.0).abs() < 1e-9);
        assert_eq!(a, None);
    }

    #[test]
    fn standalone_footprint_deinstances_back_pad_flip_and_layers() {
        // Board instance: footprint on back side, with flip + rotation baked into pad angle.
        let input = r#"
        (footprint "lib:TEST"
            (layer "B.Cu")
            (at 96.12111 46.5236 180)
            (pad "1" smd roundrect (at -2.38 6.27) (size 0.7 1.1) (layers "B.Cu" "B.Mask" "B.Paste"))
        )
        "#;

        let out = transform_board_instance_footprint_to_standalone(input).unwrap();
        let parsed = parse(&out).unwrap();

        // Root layer should be normalized to F.*.
        let root_items = parsed.as_list().unwrap();
        let root_layer = crate::find_child_list(root_items, "layer").unwrap();
        assert_eq!(root_layer.get(1).and_then(Sexpr::as_str), Some("F.Cu"));

        let mut pad: Option<(f64, f64, Option<f64>, Vec<String>)> = None;
        parsed.walk(|node, _ctx| {
            if pad.is_some() {
                return;
            }
            let Some(items) = node.as_list() else {
                return;
            };
            if items.first().and_then(Sexpr::as_sym) != Some("pad") {
                return;
            }
            if items.get(1).and_then(Sexpr::as_str) != Some("1") {
                return;
            }

            let Some(at) = crate::find_child_list(items, "at") else {
                return;
            };
            let x = at.get(1).and_then(number_as_f64).unwrap_or(f64::NAN);
            let y = at.get(2).and_then(number_as_f64).unwrap_or(f64::NAN);
            let a = at.get(3).and_then(number_as_f64);

            let Some(layers) = crate::find_child_list(items, "layers") else {
                return;
            };
            let layer_names: Vec<String> = layers
                .iter()
                .skip(1)
                .filter_map(|n| n.as_str().map(|s| s.to_string()))
                .collect();

            pad = Some((x, y, a, layer_names));
        });

        let (x, y, a, layer_names) = pad.expect("missing pad");

        // Expected local: y unflipped and local angle restored to 180.
        assert!((x - -2.38).abs() < 1e-9);
        assert!((y - -6.27).abs() < 1e-9);
        assert_eq!(a, Some(180.0));

        assert_eq!(layer_names, vec!["F.Cu", "F.Mask", "F.Paste"]);

        // Ensure we dropped justify mirror when present.
        assert!(!out.contains("justify mirror"));
    }

    #[test]
    fn standalone_footprint_deinstances_zone_points() {
        // A minimal footprint keepout zone point based on the MOLEX-5033981892 board instance.
        // p_file = t + R(180) * M(p_local)
        // where p_local = (-3.22001, 5.72989), t = (96.12111, 46.5236).
        let input = r#"
        (footprint "lib:TEST"
            (layer "B.Cu")
            (at 96.12111 46.5236 180)
            (zone
                (layer "B.Cu")
                (polygon (pts (xy 99.34112 52.25349)))
            )
        )
        "#;

        let out = transform_board_instance_footprint_to_standalone(input).unwrap();
        let parsed = parse(&out).unwrap();

        let mut xy: Option<(f64, f64)> = None;
        parsed.walk(|node, ctx| {
            if xy.is_some() {
                return;
            }
            let Some(items) = node.as_list() else {
                return;
            };
            if items.first().and_then(Sexpr::as_sym) != Some("xy") {
                return;
            }
            if ctx.parent_tag() != Some("pts") {
                return;
            }
            if ctx.grandparent_tag() != Some("polygon") {
                return;
            }

            let x = items.get(1).and_then(number_as_f64).unwrap_or(f64::NAN);
            let y = items.get(2).and_then(number_as_f64).unwrap_or(f64::NAN);
            xy = Some((x, y));
        });

        let (x, y) = xy.expect("missing polygon xy");

        assert!((x - (-3.22001)).abs() < 1e-5);
        assert!((y - 5.72989).abs() < 1e-5);
    }
}
