use super::schematic_types::ImportSchematicPositionComment;
use super::*;
use glam::{DMat2, DVec2};
use pcb_sexpr::find_child_list;
use pcb_sexpr::Sexpr;
use std::collections::BTreeMap;

const Y_FLIP: DVec2 = DVec2::new(1.0, -1.0);

// Schematic placement mapping (KiCad -> `# pcb:sch`):
// - Non-promoted symbols: preserve KiCad's persisted origin using the rendered symbol's bbox.
// - Promoted passives: substitute a different rendered symbol family (Device:R/C), so preserve
//   the sheet's visual placement by aligning the transformed visual AABB top-left between source
//   and target, then convert that target origin into the editor's stored anchor.
//
// The math model is documented in `docs/specs/kicad-import.md` ("Schematic Placement Mapping").
pub(super) struct SchematicPlacementMapper<'a> {
    symbols: SymbolGeometryDb<'a>,
}

impl<'a> SchematicPlacementMapper<'a> {
    pub(super) fn new(schematic_lib_symbols: &'a BTreeMap<KiCadLibId, String>) -> Self {
        Self {
            symbols: SymbolGeometryDb::new(schematic_lib_symbols),
        }
    }

    pub(super) fn editor_persisted_position(
        &mut self,
        position: &ImportSchematicPositionComment,
    ) -> pcb_sch::position::Position {
        let mirror = parse_mirror_axis(position.mirror.as_deref());

        let kicad_rot = position.at.rot.unwrap_or(0.0);
        let mut rot_p = kicad_rotation_deg_to_editor_persisted_rotation_deg(kicad_rot);

        let resolved_lib_id = self.symbols.resolve_symbol_lib_id(position);
        if let Some(offset_deg) =
            self.promoted_symbol_rotation_offset_deg(position, resolved_lib_id.as_ref())
        {
            rot_p = normalize_comment_rotation(rot_p + offset_deg);
        }

        let (x_s_mm, y_s_mm) =
            self.editor_stored_anchor_mm(position, resolved_lib_id.as_ref(), rot_p, mirror);

        pcb_sch::position::Position {
            // Persisted file format uses 0.1mm units.
            x: x_s_mm * 10.0,
            y: y_s_mm * 10.0,
            rotation: rot_p,
            mirror,
        }
    }

    fn editor_stored_anchor_mm(
        &mut self,
        position: &ImportSchematicPositionComment,
        resolved_source_lib_id: Option<&KiCadLibId>,
        rot_persisted_deg_cw: f64,
        mirror: Option<pcb_sch::position::MirrorAxis>,
    ) -> (f64, f64) {
        let origin_src_down = DVec2::new(position.at.x, position.at.y);

        let source_bounds = resolved_source_lib_id
            .and_then(|lib_id| self.symbols.geometry_for_unit(lib_id, position.unit))
            .and_then(|geometry| geometry.bounds);

        let target_bounds = position
            .target_kind
            .promoted_target_lib_id()
            .and_then(|lib_id| self.symbols.geometry_for_unit(&lib_id, None))
            .and_then(|geometry| geometry.bounds)
            .or(source_bounds);

        let Some(target_bounds) = target_bounds else {
            // No geometry: treat the persisted origin as the stored anchor (best-effort).
            return (origin_src_down.x, origin_src_down.y);
        };

        let origin_tgt_down = if position.target_kind.promoted_target_lib_id().is_some() {
            // For promoted passives we substitute a different symbol geometry (e.g. stdlib
            // `Device:R/C`). To preserve the sheet's visual layout, align the transformed visual
            // AABB top-left between the source and the target symbol, then treat that as the
            // substituted symbol origin.
            //
            // Transform order: rotate then mirror about origin (KiCad semantics).
            if let Some(source_bounds) = source_bounds {
                let source_rot_deg_ccw = normalize_comment_rotation(position.at.rot.unwrap_or(0.0));
                let target_rot_deg_ccw = normalize_comment_rotation(-rot_persisted_deg_cw);

                let src_linear =
                    linear_from_rotation_and_mirror_deg_ccw(source_rot_deg_ccw, mirror);
                let tgt_linear =
                    linear_from_rotation_and_mirror_deg_ccw(target_rot_deg_ccw, mirror);

                let origin_src_up = origin_src_down * Y_FLIP;
                let top_left_src_up =
                    origin_src_up + transformed_aabb_top_left(src_linear, source_bounds);

                let origin_tgt_up =
                    top_left_src_up - transformed_aabb_top_left(tgt_linear, target_bounds);
                origin_tgt_up * Y_FLIP
            } else {
                origin_src_down
            }
        } else {
            origin_src_down
        };

        kicad_origin_to_editor_stored_anchor_mm(origin_tgt_down.x, origin_tgt_down.y, target_bounds)
    }

    fn promoted_symbol_rotation_offset_deg(
        &mut self,
        position: &ImportSchematicPositionComment,
        resolved_lib_id: Option<&KiCadLibId>,
    ) -> Option<f64> {
        // Promoted passives can swap from a source symbol family (often horizontal pins)
        // to stdlib Device:R/C (vertical pins). Keep visual orientation by applying
        // the source->target pin-axis delta on top of KiCad->comment rotation conversion.
        let source_geometry = resolved_lib_id
            .and_then(|lib_id| self.symbols.geometry_for_unit(lib_id, position.unit))?;

        let target_geometry = position
            .target_kind
            .promoted_target_lib_id()
            .and_then(|lib_id| self.symbols.geometry_for_unit(&lib_id, None));

        let target_dir = target_geometry
            .and_then(|geometry| geometry.pin_1_to_2_deg)
            .or_else(|| position.target_kind.promoted_target_pin_1_to_2_deg());
        if let (Some(target_dir), Some(source_dir)) = (target_dir, source_geometry.pin_1_to_2_deg) {
            return Some(normalize_comment_rotation(target_dir - source_dir));
        }

        let target_axis = target_geometry
            .and_then(|geometry| geometry.pin_axis_deg)
            .or_else(|| position.target_kind.promoted_target_pin_axis_deg())?;
        let source_axis = source_geometry.pin_axis_deg?;
        Some(normalize_passive_axis_offset_deg(target_axis, source_axis))
    }
}

pub(super) fn format_pcb_sch_comment_line(
    element_id: &str,
    position: &pcb_sch::position::Position,
) -> String {
    let mirror_suffix = position
        .mirror
        .map(|axis| format!(" mirror={}", axis.as_comment_value()))
        .unwrap_or_default();
    format!(
        "# pcb:sch {element_id} x={:.4} y={:.4} rot={:.0}{}\n",
        position.x, position.y, position.rotation, mirror_suffix
    )
}

fn parse_mirror_axis(value: Option<&str>) -> Option<pcb_sch::position::MirrorAxis> {
    let v = value?;
    if v.eq_ignore_ascii_case("x") {
        Some(pcb_sch::position::MirrorAxis::X)
    } else if v.eq_ignore_ascii_case("y") {
        Some(pcb_sch::position::MirrorAxis::Y)
    } else {
        None
    }
}

fn linear_from_rotation_and_mirror_deg_ccw(
    rot_deg_ccw: f64,
    mirror: Option<pcb_sch::position::MirrorAxis>,
) -> DMat2 {
    // Column-vector convention:
    // q' = M(mirror) * (R(theta) * q)  => linear = M * R
    let rot = DMat2::from_angle(rot_deg_ccw.to_radians());
    let mirror_mat = match mirror {
        Some(pcb_sch::position::MirrorAxis::X) => DMat2::from_diagonal(DVec2::new(1.0, -1.0)),
        Some(pcb_sch::position::MirrorAxis::Y) => DMat2::from_diagonal(DVec2::new(-1.0, 1.0)),
        None => DMat2::IDENTITY,
    };
    mirror_mat * rot
}

fn transformed_aabb_top_left(linear: DMat2, bounds: SymbolLocalBounds) -> DVec2 {
    // "Visual AABB" top-left in Y-up: (min_x, max_y) after applying the instance linear transform.
    //
    // Corner set is sufficient: for an affine transform, extrema over a rectangle occur at
    // vertices. We only need consistent bbox alignment, not exact primitive bounds.
    let corners = [
        DVec2::new(bounds.min.x, bounds.min.y),
        DVec2::new(bounds.min.x, bounds.max.y),
        DVec2::new(bounds.max.x, bounds.min.y),
        DVec2::new(bounds.max.x, bounds.max.y),
    ];

    let mut min = DVec2::splat(f64::INFINITY);
    let mut max = DVec2::splat(f64::NEG_INFINITY);
    for corner in corners {
        let p = linear * corner;
        min = min.min(p);
        max = max.max(p);
    }

    DVec2::new(min.x, max.y)
}

fn kicad_origin_to_editor_stored_anchor_mm(
    kicad_origin_x_mm_y_down: f64,
    kicad_origin_y_mm_y_down: f64,
    bounds: SymbolLocalBounds,
) -> (f64, f64) {
    // See docs/specs/kicad-import.md: "Required mapping for visual parity".
    (
        kicad_origin_x_mm_y_down + bounds.min.x,
        kicad_origin_y_mm_y_down - bounds.max.y,
    )
}

fn kicad_rotation_deg_to_editor_persisted_rotation_deg(kicad_rotation_deg: f64) -> f64 {
    // The editor stores clockwise-positive degrees and loads with `world_rot = -stored_rot`.
    // Emit the inverse so the editor's world rotation matches KiCad.
    normalize_comment_rotation(-kicad_rotation_deg)
}

fn normalize_comment_rotation(rotation: f64) -> f64 {
    let normalized = rotation.rem_euclid(360.0);
    if normalized.abs() < f64::EPSILON || (360.0 - normalized).abs() < f64::EPSILON {
        0.0
    } else {
        normalized
    }
}

fn normalize_passive_axis_offset_deg(target_axis: f64, source_axis: f64) -> f64 {
    let offset = (target_axis - source_axis).rem_euclid(180.0);
    if offset.abs() < f64::EPSILON || (180.0 - offset).abs() < f64::EPSILON {
        0.0
    } else {
        offset
    }
}

struct SymbolGeometryDb<'a> {
    embedded_lib_symbols: &'a BTreeMap<KiCadLibId, String>,
    geometry_cache: BTreeMap<(KiCadLibId, Option<i64>), Option<SymbolLocalGeometry>>,
    global_symbol_cache: BTreeMap<KiCadLibId, Option<String>>,
    kicad_symbol_dir: Option<std::path::PathBuf>,
}

impl<'a> SymbolGeometryDb<'a> {
    fn new(embedded_lib_symbols: &'a BTreeMap<KiCadLibId, String>) -> Self {
        Self {
            embedded_lib_symbols,
            geometry_cache: BTreeMap::new(),
            global_symbol_cache: BTreeMap::new(),
            kicad_symbol_dir: find_kicad_symbol_dir(),
        }
    }

    fn resolve_symbol_lib_id(
        &self,
        position: &ImportSchematicPositionComment,
    ) -> Option<KiCadLibId> {
        position
            .lib_name
            .as_ref()
            .map(|n| KiCadLibId::from(n.clone()))
            .filter(|id| self.embedded_lib_symbols.contains_key(id))
            .or_else(|| position.lib_id.clone())
    }

    fn geometry_for_unit(
        &mut self,
        lib_id: &KiCadLibId,
        unit: Option<i64>,
    ) -> Option<SymbolLocalGeometry> {
        let cache_key = (lib_id.clone(), unit);
        if let Some(geometry) = self.geometry_cache.get(&cache_key) {
            return *geometry;
        }

        let computed = self
            .raw_symbol_for_lib_id(lib_id)
            .and_then(|raw| extract_symbol_local_geometry(raw, unit));
        self.geometry_cache.insert(cache_key, computed);
        computed
    }

    fn raw_symbol_for_lib_id(&mut self, lib_id: &KiCadLibId) -> Option<&str> {
        if let Some(raw) = self.embedded_lib_symbols.get(lib_id) {
            return Some(raw.as_str());
        }

        if !self.global_symbol_cache.contains_key(lib_id) {
            let loaded = load_symbol_from_kicad_global_library(
                self.kicad_symbol_dir.as_deref(),
                lib_id.as_str(),
            );
            self.global_symbol_cache.insert(lib_id.clone(), loaded);
        }

        self.global_symbol_cache
            .get(lib_id)
            .and_then(|value| value.as_deref())
    }
}

#[derive(Debug, Clone, Copy)]
struct SymbolLocalGeometry {
    bounds: Option<SymbolLocalBounds>,
    pin_axis_deg: Option<f64>,
    pin_1_to_2_deg: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct SymbolLocalBounds {
    min: DVec2,
    max: DVec2,
}

impl SymbolLocalBounds {
    fn from_point(p: DVec2) -> Self {
        Self { min: p, max: p }
    }

    fn include_point(&mut self, p: DVec2) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    fn expand(&mut self, margin: f64) {
        let d = DVec2::splat(margin);
        self.min -= d;
        self.max += d;
    }
}

fn extract_symbol_local_geometry(
    raw_symbol: &str,
    target_unit: Option<i64>,
) -> Option<SymbolLocalGeometry> {
    // Keep this in sync with `editor_graphics::calculate_symbol_bbox()` expansion.
    const EXPANSION_MM: f64 = 0.1;

    let parsed = pcb_sexpr::parse(raw_symbol).ok()?;
    let symbol = parsed.as_list()?;
    if symbol.first().and_then(Sexpr::as_sym) != Some("symbol") {
        return None;
    }

    let mut bounds: Option<SymbolLocalBounds> = None;
    let mut pins: Vec<(Option<String>, DVec2)> = Vec::new();
    collect_symbol_bounds(symbol, target_unit, DVec2::ZERO, true, &mut bounds);
    collect_symbol_pin_points(symbol, target_unit, DVec2::ZERO, true, &mut pins);
    if let Some(ref mut bounds) = bounds {
        bounds.expand(EXPANSION_MM);
    }
    let pin_axis_deg = first_two_distinct_pin_points(&pins).map(|(first, second)| {
        let d = second - first;
        d.y.atan2(d.x).to_degrees().rem_euclid(180.0)
    });

    let pin_1_to_2_deg = pin_1_to_2_points(&pins).map(|(p1, p2)| {
        let d = p2 - p1;
        d.y.atan2(d.x).to_degrees().rem_euclid(360.0)
    });

    Some(SymbolLocalGeometry {
        bounds,
        pin_axis_deg,
        pin_1_to_2_deg,
    })
}

fn find_kicad_symbol_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    let possible_paths = if cfg!(target_os = "macos") {
        vec![
            PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport/symbols"),
            PathBuf::from("/Library/Application Support/kicad/symbols"),
            dirs::home_dir()
                .map(|h| h.join("Library/Application Support/kicad/symbols"))
                .unwrap_or_default(),
        ]
    } else if cfg!(target_os = "windows") {
        vec![
            PathBuf::from("C:\\Program Files\\KiCad\\share\\kicad\\symbols"),
            PathBuf::from("C:\\Program Files (x86)\\KiCad\\share\\kicad\\symbols"),
            dirs::config_dir()
                .map(|c| c.join("kicad\\symbols"))
                .unwrap_or_default(),
        ]
    } else {
        vec![
            PathBuf::from("/usr/share/kicad/symbols"),
            PathBuf::from("/usr/local/share/kicad/symbols"),
            PathBuf::from("/opt/kicad/share/kicad/symbols"),
            dirs::home_dir()
                .map(|h| h.join(".local/share/kicad/symbols"))
                .unwrap_or_default(),
        ]
    };

    if let Ok(env_path) = std::env::var("KICAD_SYMBOL_DIR") {
        let mut paths = vec![PathBuf::from(env_path)];
        paths.extend(possible_paths);
        return paths.into_iter().find(|p| p.exists());
    }

    possible_paths.into_iter().find(|p| p.exists())
}

fn load_symbol_from_kicad_global_library(
    kicad_symbol_dir: Option<&std::path::Path>,
    lib_id: &str,
) -> Option<String> {
    use std::fs;

    let (library_name, symbol_name) = lib_id.split_once(':')?;
    let kicad_symbol_dir = kicad_symbol_dir?;
    let lib_path = kicad_symbol_dir.join(format!("{library_name}.kicad_sym"));
    let content = fs::read_to_string(&lib_path).ok()?;
    let sexpr = pcb_sexpr::parse(&content).ok()?;

    let symbol = find_symbol_in_kicad_library(&sexpr, symbol_name)?;
    Some(pcb_sexpr::format_sexpr(symbol, 0))
}

fn find_symbol_in_kicad_library<'a>(root: &'a Sexpr, symbol_name: &str) -> Option<&'a Sexpr> {
    let items = root.as_list()?;
    // KiCad symbol libs look like:
    // (kicad_symbol_lib ... (symbol "R" ...) (symbol "C" ...) ...)
    for node in items.iter().skip(1) {
        let Some(symbol_items) = node.as_list() else {
            continue;
        };
        if symbol_items.first().and_then(Sexpr::as_sym) != Some("symbol") {
            continue;
        }
        let Some(name) = symbol_items.get(1).and_then(Sexpr::as_atom) else {
            continue;
        };
        if name == symbol_name {
            return Some(node);
        }
    }
    None
}

fn collect_symbol_pin_points(
    symbol: &[Sexpr],
    target_unit: Option<i64>,
    parent_offset: DVec2,
    include_here: bool,
    pins: &mut Vec<(Option<String>, DVec2)>,
) {
    let local_offset = parse_xy_from_child(symbol, "at").unwrap_or(DVec2::ZERO);
    let total_offset = parent_offset + local_offset;

    for node in symbol.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        let Some(tag) = items.first().and_then(Sexpr::as_sym) else {
            continue;
        };

        if tag == "symbol" {
            let include_nested = include_here && symbol_matches_target_unit(items, target_unit);
            collect_symbol_pin_points(items, target_unit, total_offset, include_nested, pins);
            continue;
        }

        if !include_here || tag != "pin" {
            continue;
        }

        if let Some(p) = parse_xy_from_child(items, "at") {
            pins.push((parse_pin_number(items), total_offset + p));
        }
    }
}

fn parse_pin_number(pin: &[Sexpr]) -> Option<String> {
    find_child_list(pin, "number")
        .and_then(|n| n.get(1))
        .and_then(Sexpr::as_atom)
        .map(|s| s.to_string())
}

fn first_two_distinct_pin_points(pins: &[(Option<String>, DVec2)]) -> Option<(DVec2, DVec2)> {
    let first = pins.first()?.1;
    let second = pins.iter().skip(1).map(|(_, p)| *p).find(|p| *p != first)?;
    Some((first, second))
}

fn pin_1_to_2_points(pins: &[(Option<String>, DVec2)]) -> Option<(DVec2, DVec2)> {
    let p1 = pins
        .iter()
        .find(|(n, _)| n.as_deref() == Some("1"))
        .map(|(_, p)| *p)?;
    let p2 = pins
        .iter()
        .find(|(n, _)| n.as_deref() == Some("2"))
        .map(|(_, p)| *p)?;
    if p1 == p2 {
        return None;
    }
    Some((p1, p2))
}

fn collect_symbol_bounds(
    symbol: &[Sexpr],
    target_unit: Option<i64>,
    parent_offset: DVec2,
    include_here: bool,
    bounds: &mut Option<SymbolLocalBounds>,
) {
    let local_offset = parse_xy_from_child(symbol, "at").unwrap_or(DVec2::ZERO);
    let total_offset = parent_offset + local_offset;

    for node in symbol.iter().skip(1) {
        let Some(items) = node.as_list() else {
            continue;
        };
        let Some(tag) = items.first().and_then(Sexpr::as_sym) else {
            continue;
        };

        if tag == "symbol" {
            let include_nested = include_here && symbol_matches_target_unit(items, target_unit);
            collect_symbol_bounds(items, target_unit, total_offset, include_nested, bounds);
            continue;
        }

        if !include_here {
            continue;
        }

        include_primitive_points(items, total_offset, bounds);
    }
}

fn symbol_matches_target_unit(symbol: &[Sexpr], target_unit: Option<i64>) -> bool {
    let Some(name) = symbol.get(1).and_then(Sexpr::as_atom) else {
        return true;
    };
    let Some(unit) = parse_symbol_name_unit_suffix(name) else {
        return true;
    };
    unit == 0 || target_unit.is_none_or(|target| target == unit)
}

fn parse_symbol_name_unit_suffix(name: &str) -> Option<i64> {
    let (prefix, body_style) = name.rsplit_once('_')?;
    if !body_style.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (_, unit) = prefix.rsplit_once('_')?;
    if !unit.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    unit.parse::<i64>().ok()
}

fn include_primitive_points(
    primitive: &[Sexpr],
    offset: DVec2,
    bounds: &mut Option<SymbolLocalBounds>,
) {
    let Some(tag) = primitive.first().and_then(Sexpr::as_sym) else {
        return;
    };

    match tag {
        "rectangle" => {
            if let Some(p) = parse_xy_from_child(primitive, "start") {
                include_symbol_point(bounds, offset + p);
            }
            if let Some(p) = parse_xy_from_child(primitive, "end") {
                include_symbol_point(bounds, offset + p);
            }
        }
        "polyline" | "bezier" => {
            if let Some(pts) = find_child_list(primitive, "pts") {
                for pt in pts.iter().skip(1) {
                    let Some(pt_items) = pt.as_list() else {
                        continue;
                    };
                    if pt_items.first().and_then(Sexpr::as_sym) != Some("xy") {
                        continue;
                    }
                    let Some(x) = pt_items.get(1).and_then(sexpr_number) else {
                        continue;
                    };
                    let Some(y) = pt_items.get(2).and_then(sexpr_number) else {
                        continue;
                    };
                    include_symbol_point(bounds, offset + DVec2::new(x, y));
                }
            }
        }
        "circle" => {
            let Some(center) = parse_xy_from_child(primitive, "center") else {
                return;
            };
            let Some(radius) = find_child_list(primitive, "radius")
                .and_then(|r| r.get(1))
                .and_then(sexpr_number)
            else {
                return;
            };
            include_symbol_point(
                bounds,
                offset + DVec2::new(center.x - radius, center.y - radius),
            );
            include_symbol_point(
                bounds,
                offset + DVec2::new(center.x + radius, center.y + radius),
            );
        }
        "arc" => {
            for key in ["start", "mid", "end"] {
                if let Some(p) = parse_xy_from_child(primitive, key) {
                    include_symbol_point(bounds, offset + p);
                }
            }
        }
        "pin" => {
            let Some(at) = find_child_list(primitive, "at") else {
                return;
            };
            let Some(x) = at.get(1).and_then(sexpr_number) else {
                return;
            };
            let Some(y) = at.get(2).and_then(sexpr_number) else {
                return;
            };
            include_symbol_point(bounds, offset + DVec2::new(x, y));

            let length = find_child_list(primitive, "length")
                .and_then(|v| v.get(1))
                .and_then(sexpr_number)
                .unwrap_or(0.0);
            let angle_deg = at.get(3).and_then(sexpr_number).unwrap_or(0.0);
            let angle = angle_deg.to_radians();
            include_symbol_point(
                bounds,
                offset + DVec2::new(x + length * angle.cos(), y + length * angle.sin()),
            );
        }
        _ => {}
    }
}

fn parse_xy_from_child(list: &[Sexpr], key: &str) -> Option<DVec2> {
    let node = find_child_list(list, key)?;
    let x = node.get(1).and_then(sexpr_number)?;
    let y = node.get(2).and_then(sexpr_number)?;
    Some(DVec2::new(x, y))
}

fn sexpr_number(node: &Sexpr) -> Option<f64> {
    node.as_float().or_else(|| node.as_int().map(|v| v as f64))
}

fn include_symbol_point(bounds: &mut Option<SymbolLocalBounds>, p: DVec2) {
    match bounds {
        Some(existing) => existing.include_point(p),
        None => *bounds = Some(SymbolLocalBounds::from_point(p)),
    }
}
