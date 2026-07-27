use super::*;
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};

/// Two lengths in millimetres are the same pad dimension within this tolerance.
///
/// KiCad writes pad geometry as decimal millimetres, so equal land patterns authored by different
/// tools differ only in float formatting. One nanometre is far below any manufacturable tolerance and
/// far above float-formatting noise.
const LENGTH_EPSILON_MM: f64 = 1e-6;

/// Pad fields this comparison deliberately does not read, from a survey of 25,000 real footprints
/// (647,000 pads): `uuid` and `property` identify or annotate the pad; `clearance`, the solder mask and
/// paste margins, `zone_connect` and `thermal_bridge_angle` are fabrication rules applied *to* the
/// copper rather than its shape; `remove_unused_layers` and `keep_end_layers` govern which inner layers
/// of a multilayer board the barrel touches, which a land pattern does not describe — and at 60% of
/// files, comparing them would reject almost everything for no physical reason.
///
/// Keys whose presence makes a footprint incomparable rather than being read.
///
/// A `custom` pad's outline lives in `primitives`, anchored per `options`. Instead of comparing an
/// anchor size and calling two arbitrary shapes equal, [`has_incomparable_pad`] reports the whole
/// footprint as having no comparable geometry and the caller falls back to comparing names.
const PAD_FIELDS_REFUSED: [&str; 2] = ["options", "primitives"];

/// One pad reduced to what makes a land pattern physically the same.
///
/// Deliberately excluded: `uuid`, net assignment, solder-mask and paste layers, mask/paste margins,
/// thermal relief, and the pad's own name beyond its number. Those differ freely between libraries
/// describing one physical part — the `ublox_SAM-M8Q` and `SAM-M10Q-00B` libraries for the same module
/// differ only by an `F.Paste` layer — and none of them changes where copper meets the board.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PadLand {
    /// `smd`, `thru_hole`, `np_thru_hole`, ... — a through-hole pad is not an SMD pad.
    kind: String,
    /// `rect`, `circle`, `oval`, `roundrect`, ... A `roundrect` with no corner radius is normalized to
    /// `rect`, because that is the same copper written two ways. Otherwise compared strictly: treating
    /// `rect` and `oval` as interchangeable would let a substitution change the outline.
    shape: String,
    /// `roundrect_rratio`, the corner radius as a fraction of the shorter side; zero for a square
    /// corner. Part of identity: two `roundrect` pads of the same size and different radius are
    /// different copper, and the corpus carries 26 distinct values.
    corner_ratio: f64,
    /// `chamfer_ratio` and the `chamfer` corner list, which cut one or more corners off the outline.
    ///
    /// These matter more than their rarity suggests: a chamfered pad is written as a `roundrect` with
    /// `(roundrect_rratio 0)`, which the shape normalization below would otherwise reduce to a plain
    /// `rect` — making a cut corner invisible.
    chamfer_ratio: f64,
    chamfer_corners: BTreeSet<String>,
    x_mm: f64,
    y_mm: f64,
    /// Extent across the pad's own X axis after [`normalize_rotation`].
    width_mm: f64,
    /// Extent across the pad's own Y axis after [`normalize_rotation`].
    height_mm: f64,
    /// Residual rotation in `[0, 180)` after the axis swap. Zero for the common right-angle cases, and
    /// always zero for a `circle`, whose outline no rotation changes.
    rotation_deg: f64,
    /// Copper sides the pad occupies, with `*.Cu` expanded so the wildcard and the spelled-out pair
    /// compare equal. Which side a pad is on is physical; which mask layers it also touches is not.
    copper: BTreeSet<String>,
    /// The hole's `(drill ...)` dimensions in millimetres, empty for a surface-mount pad.
    ///
    /// Part of identity: two through-hole pads can present the same copper and take a different hole,
    /// which is a different part. Ignoring it let a substitution reuse a module whose lead diameter
    /// does not fit.
    drill_mm: Vec<f64>,
    /// The hole's `(offset ...)` within the pad. A shifted hole puts the lead somewhere else relative to
    /// the copper, so it is identity even though it is rare — `FlexyPin` uses it to bias every hole.
    drill_offset_mm: Vec<f64>,
}

impl PadLand {
    fn matches(&self, other: &PadLand) -> bool {
        self.kind == other.kind
            && self.shape == other.shape
            && self.copper == other.copper
            && close(self.corner_ratio, other.corner_ratio)
            && close(self.chamfer_ratio, other.chamfer_ratio)
            && self.chamfer_corners == other.chamfer_corners
            && close(self.x_mm, other.x_mm)
            && close(self.y_mm, other.y_mm)
            && close(self.width_mm, other.width_mm)
            && close(self.height_mm, other.height_mm)
            && close(self.rotation_deg, other.rotation_deg)
            && same_lengths(&self.drill_mm, &other.drill_mm)
            && same_lengths(&self.drill_offset_mm, &other.drill_offset_mm)
    }
}

fn close(left: f64, right: f64) -> bool {
    (left - right).abs() <= LENGTH_EPSILON_MM
}

fn same_lengths(left: &[f64], right: &[f64]) -> bool {
    left.len() == right.len() && std::iter::zip(left, right).all(|(a, b)| close(*a, *b))
}

/// Fold a pad's rotation into its dimensions so that one land pattern has one representation.
///
/// A rectangle 1.8 x 1.5 at 0 degrees and a rectangle 1.5 x 1.8 at 90 degrees are the same copper.
/// Real libraries disagree on which they write — the two u-blox SAM-M10Q footprints in this repo
/// differ in exactly that way on all 20 pads — so comparing the literal `(size ...)` and `(at ...)`
/// values rejects footprints that are physically identical.
///
/// A right angle swaps the axes; a half turn leaves them alone. Any other angle is kept as residual
/// rotation, reduced modulo 180 degrees because a symmetric pad outline is unchanged by a half turn.
///
/// Only for a `symmetric` pad — one with no chamfered corner and no drill offset. Those features point
/// somewhere, so folding the angle away would compare two different pads as equal.
fn normalize_rotation(pad: &mut PadLand, rotation: f64, symmetric: bool) {
    if !symmetric {
        // Compared at its literal angle, modulo a full turn. Folding would be unsound: a half turn
        // moves a chamfered corner to the opposite corner and an offset hole to the opposite side, so
        // two physically different pads would match, while a valid quarter-turn rewrite of an offset
        // hole would not. Transforming the corner names and the offset vector correctly is possible but
        // is its own source of sign errors, and both features are rare — 71 chamfered and 47 offset
        // pads among 647,000 surveyed.
        pad.rotation_deg = rotation.rem_euclid(360.0);
        return;
    }

    let reduced = rotation.rem_euclid(180.0);
    if close(reduced, 90.0) {
        // A quarter turn exchanges the pad's own axes, so every dimension expressed in that frame swaps
        // with it — the copper extents and an oval hole's two dimensions alike. Missing the hole
        // rejected a `1.5 x 1.8` pad drilled `oval 0.6 1.2` against the identical `1.8 x 1.5` pad
        // drilled `oval 1.2 0.6`.
        std::mem::swap(&mut pad.width_mm, &mut pad.height_mm);
        if pad.drill_mm.len() == 2 {
            pad.drill_mm.swap(0, 1);
        }
        pad.rotation_deg = 0.0;
    } else if close(reduced, 0.0) {
        pad.rotation_deg = 0.0;
    } else {
        pad.rotation_deg = reduced;
    }
}

/// The numbered pads of a `.kicad_mod` or embedded `(footprint ...)`, grouped by pad number.
///
/// Grouped rather than keyed one-to-one: a single number can name *several* physical pads — a split
/// thermal pad, a via array under a ground pad — and how many there are and where they sit is part of
/// the land pattern. Keeping one pad per number silently discarded the rest, so a footprint with 55
/// pads numbered "25" (real: `Quectel_LG290P`) compared equal to one with a single pad numbered "25".
///
/// Pads with no number (mechanical, courtyard-only) are skipped: they carry no connectivity and
/// libraries add or omit them freely.
pub(super) fn pad_lands(source: &str) -> Result<BTreeMap<KiCadPinNumber, Vec<PadLand>>> {
    let root = pcb_sexpr::parse(source).map_err(|error| anyhow::anyhow!(error))?;
    let mut lands: BTreeMap<KiCadPinNumber, Vec<PadLand>> = BTreeMap::new();

    for pad in root.find_all_lists("pad") {
        let text_at = |index: usize| {
            pad.get(index)
                .and_then(|value| value.as_str().or_else(|| value.as_sym()))
        };
        let Some(number) = text_at(1).filter(|number| !number.is_empty()) else {
            continue;
        };
        let (Some(kind), Some(shape)) = (text_at(2), text_at(3)) else {
            continue;
        };

        let at = numbers_in(pad, "at");
        let size = numbers_in(pad, "size");
        let (Some(&x), Some(&y)) = (at.first(), at.get(1)) else {
            continue;
        };
        let (Some(&width), Some(&height)) = (size.first(), size.get(1)) else {
            continue;
        };
        let corner_ratio = numbers_in(pad, "roundrect_rratio")
            .first()
            .copied()
            .unwrap_or(0.0);
        let chamfer_ratio = numbers_in(pad, "chamfer_ratio")
            .first()
            .copied()
            .unwrap_or(0.0);
        let chamfer_corners = pcb_sexpr::find_child_list(pad, "chamfer")
            .unwrap_or_default()
            .iter()
            .skip(1)
            .filter_map(|value| value.as_str().or_else(|| value.as_sym()))
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        // A `rect` is a `roundrect` with no corner radius; libraries write the same copper either way.
        // Only when nothing else shapes the corners: a chamfered pad is also written with
        // `(roundrect_rratio 0)`, and calling that a plain rect would hide the cut corner.
        let squared_off =
            close(corner_ratio, 0.0) && close(chamfer_ratio, 0.0) && chamfer_corners.is_empty();
        let shape = if shape == "roundrect" && squared_off {
            "rect"
        } else {
            shape
        };

        // `*.Cu` means every copper layer, which a through-hole pad is on. Libraries write either the
        // wildcard or the outer pair for the same pad, so the wildcard is expanded rather than compared
        // as a literal string.
        let drill = pcb_sexpr::find_child_list(pad, "drill").unwrap_or_default();
        let drill_offset_mm = pcb_sexpr::find_child_list(drill, "offset")
            .unwrap_or_default()
            .iter()
            .skip(1)
            .filter_map(as_f64)
            .collect::<Vec<f64>>();
        // Whether the pad's copper and hole look the same after a turn. A chamfer cuts one named corner
        // and a drill offset points one way, so neither survives folding the angle away.
        let symmetric = chamfer_corners.is_empty() && drill_offset_mm.is_empty();

        let copper = pcb_sexpr::find_child_list(pad, "layers")
            .unwrap_or_default()
            .iter()
            .skip(1)
            .filter_map(|value| value.as_str().or_else(|| value.as_sym()))
            .filter(|layer| layer.ends_with(".Cu"))
            .flat_map(|layer| match layer {
                "*.Cu" => vec!["B.Cu".to_string(), "F.Cu".to_string()],
                named => vec![named.to_string()],
            })
            .collect::<BTreeSet<_>>();

        let mut land = PadLand {
            kind: kind.to_string(),
            shape: shape.to_string(),
            corner_ratio,
            chamfer_ratio,
            chamfer_corners,
            x_mm: x,
            y_mm: y,
            width_mm: width,
            height_mm: height,
            rotation_deg: 0.0,
            copper,
            drill_mm: drill.iter().skip(1).filter_map(as_f64).collect(),
            drill_offset_mm,
        };
        normalize_rotation(&mut land, at.get(2).copied().unwrap_or(0.0), symmetric);
        // A circle's outline is unchanged by any rotation, so an angle on one carries no information —
        // unless its hole is offset, in which case turning the pad moves the hole.
        if land.shape == "circle" && symmetric {
            land.rotation_deg = 0.0;
        }

        lands
            .entry(KiCadPinNumber::from(number.to_string()))
            .or_default()
            .push(land);
    }

    if lands.is_empty() {
        anyhow::bail!("footprint source declares no numbered pads");
    }
    Ok(lands)
}

/// The numeric arguments of `(key ...)` inside a pad.
///
/// KiCad writes these as bare numbers, which the parser gives as `Int` or `F64` rather than text, so
/// a string-only reader silently sees no coordinates at all and every pad looks unreadable.
fn numbers_in(pad: &[pcb_sexpr::Sexpr], key: &str) -> Vec<f64> {
    pcb_sexpr::find_child_list(pad, key)
        .unwrap_or_default()
        .iter()
        .skip(1)
        .filter_map(as_f64)
        .collect()
}

fn as_f64(value: &pcb_sexpr::Sexpr) -> Option<f64> {
    match &value.kind {
        pcb_sexpr::SexprKind::F64(number) => Some(*number),
        pcb_sexpr::SexprKind::Int(number) => Some(*number as f64),
        // Some writers quote coordinates.
        pcb_sexpr::SexprKind::String(text) | pcb_sexpr::SexprKind::Symbol(text) => {
            text.parse::<f64>().ok()
        }
        pcb_sexpr::SexprKind::List(_) => None,
    }
}

/// Whether two footprint sources describe the same land pattern.
///
/// This is the physical question a registry substitution actually turns on: will the candidate's
/// copper land where the source design's copper lands. It replaces comparing footprint *names*, which
/// says nothing — the same u-blox module is `ublox_SAM-M8Q` in one library and `SAM-M10Q-00B` in
/// another, while two genuinely different parts can share a name across libraries.
pub(super) fn same_land_pattern(source: &str, candidate: &str) -> Result<bool> {
    let source_lands = pad_lands(source).context("failed to read source footprint pads")?;
    let candidate_lands =
        pad_lands(candidate).context("failed to read candidate footprint pads")?;

    if source_lands.len() != candidate_lands.len() {
        return Ok(false);
    }
    Ok(source_lands.iter().all(|(number, source_pads)| {
        candidate_lands
            .get(number)
            .is_some_and(|candidate_pads| same_pads(source_pads, candidate_pads))
    }))
}

/// Whether the pads sharing one number are the same set of pads.
///
/// Matched one-for-one rather than in order, because two libraries may list a thermal array's pads in
/// any order. Pad equality is within a tolerance, so this pairs greedily; for pads that are equal to
/// within a nanometre any pairing gives the same answer.
fn same_pads(source: &[PadLand], candidate: &[PadLand]) -> bool {
    if source.len() != candidate.len() {
        return false;
    }
    let mut taken = vec![false; candidate.len()];
    for pad in source {
        let Some(index) = candidate
            .iter()
            .enumerate()
            .find_map(|(index, other)| (!taken[index] && pad.matches(other)).then_some(index))
        else {
            return false;
        };
        taken[index] = true;
    }
    true
}

/// Whether the footprint has a pad whose copper this comparison cannot see.
///
/// A `custom` pad's outline lives in `(primitives ...)`, which is not read — only its anchor `size` and
/// the `custom` token would be compared, so two arbitrarily different shapes looked identical. Rather
/// than compare geometry it cannot see, the caller treats such a footprint as having no comparable
/// geometry and falls back to comparing footprint names: weaker evidence, but never a false match.
///
/// Rare in practice: 46 pads across 35 of 40,000 surveyed footprints.
fn has_incomparable_pad(source: &str) -> bool {
    let Ok(root) = pcb_sexpr::parse(source) else {
        return false;
    };
    root.find_all_lists("pad").into_iter().any(|pad| {
        // Keyed on the fields rather than on the `custom` shape token: the outline is incomparable
        // exactly when it is described by something unread, whatever the shape happens to be called.
        PAD_FIELDS_REFUSED
            .iter()
            .any(|field| pcb_sexpr::find_child_list(pad, field).is_some())
    })
}

/// The footprint source for a component in library form, when import resolved geometry for it.
///
/// A `BoardInstance` is de-instanced first. A footprint embedded in a `.kicad_pcb` carries *absolute*
/// pad angles — the placement rotation is folded into each pad — while a library `.kicad_mod` stores
/// local ones. Comparing the raw board text against a library candidate therefore fails for any
/// footprint placed at a non-zero angle, even when the copper is identical. Generation already
/// de-instances before writing a `.kicad_mod`; this is the same conversion for the same reason.
///
/// `None` when there is nothing comparable: `StandardLibrary` is a bundled-stdlib reference with no
/// local text, `Unresolved` has no geometry, a board instance that will not convert is treated as
/// absent, and so is a footprint carrying a `custom` pad — see [`has_incomparable_pad`]. In each case
/// the caller falls back to comparing names rather than to comparing geometry it cannot trust.
pub(super) fn component_footprint_source(
    component: &ImportComponentData,
) -> Option<std::borrow::Cow<'_, str>> {
    match &component.layout.as_ref()?.footprint_geometry {
        ImportFootprintGeometry::LibraryFile(source) => {
            (!has_incomparable_pad(source)).then_some(std::borrow::Cow::Borrowed(source.as_str()))
        }
        ImportFootprintGeometry::BoardInstance(source) => {
            match pcb_sexpr::board::transform_board_instance_footprint_to_standalone(source) {
                Ok(standalone) if !has_incomparable_pad(&standalone) => {
                    Some(std::borrow::Cow::Owned(standalone))
                }
                Ok(_) => None,
                Err(error) => {
                    log::debug!("Board-instance footprint will not de-instance: {error}");
                    None
                }
            }
        }
        ImportFootprintGeometry::StandardLibrary | ImportFootprintGeometry::Unresolved => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    /// A plain two-pad SMD footprint; the cases below vary from it.
    const BASE: &str = r#"(footprint "b" (layer "F.Cu")
        (pad "1" smd rect (at -6.6 -3.8) (size 1.8 1.5) (layers "F.Cu" "F.Mask"))
        (pad "2" smd rect (at 6.6 3.8) (size 1.8 1.5) (layers "F.Cu" "F.Mask")))"#;

    /// Notation two libraries disagree on while describing the same copper.
    ///
    /// This is why the comparison is not a string compare. Every case here was a footprint wrongly
    /// rejected at some point, which silently costs a registry substitution.
    #[test]
    fn equivalent_notation_is_the_same_land_pattern() {
        // A quarter turn exchanges the pad's axes, so its dimensions swap. The two u-blox SAM-M10Q
        // libraries differ in exactly this way on all twenty pads.
        let turned = r#"(footprint "a" (layer "F.Cu")
        (pad "1" smd rect (at -6.6 -3.8 90) (size 1.5 1.8) (layers "F.Cu" "F.Mask" "F.Paste"))
        (pad "2" smd rect (at 6.6 3.8 90) (size 1.5 1.8) (layers "F.Cu" "F.Mask" "F.Paste")))"#;
        assert!(same_land_pattern(BASE, turned).unwrap(), "quarter turn");
        assert!(
            same_land_pattern(turned, BASE).unwrap(),
            "and symmetrically"
        );

        let roundrect_zero = BASE.replace("smd rect", "smd roundrect").replace(
            r#"(layers "F.Cu" "F.Mask"))"#,
            r#"(layers "F.Cu" "F.Mask") (roundrect_rratio 0))"#,
        );
        let mechanical = BASE.replace(
            r#"(pad "2""#,
            r#"(pad "" smd rect (at 3 3) (size 2 2) (layers "F.Cu")) (pad "2""#,
        );
        for (case, source) in [
            (
                "half turn",
                BASE.replace("(at -6.6 -3.8)", "(at -6.6 -3.8 180)"),
            ),
            (
                "extra paste layer",
                BASE.replace(r#""F.Mask""#, r#""F.Mask" "F.Paste""#),
            ),
            ("zero-radius roundrect is a rect", roundrect_zero),
            ("added unnumbered mechanical pad", mechanical),
        ] {
            assert!(same_land_pattern(BASE, &source).unwrap(), "{case}");
        }

        // `*.Cu` and the spelled-out outer pair describe one through-hole pad, and no rotation changes
        // a circle's outline.
        let hole = r#"(footprint "t" (layer "F.Cu")
        (pad "1" thru_hole circle (at 0 0) (size 1.6 1.6) (drill 0.8) (layers "*.Cu" "*.Mask")))"#;
        for (case, source) in [
            (
                "*.Cu equals F.Cu + B.Cu",
                hole.replace(r#""*.Cu""#, r#""F.Cu" "B.Cu""#),
            ),
            ("rotated circle", hole.replace("(at 0 0)", "(at 0 0 37)")),
        ] {
            assert!(same_land_pattern(hole, &source).unwrap(), "{case}");
        }

        // A slot's dimensions swap with the pad's on a quarter turn.
        let slot = r#"(footprint "s" (layer "F.Cu")
        (pad "1" thru_hole oval (at 0 0) (size 1.8 1.5) (drill oval 1.2 0.6) (layers "*.Cu")))"#;
        let slot_turned = r#"(footprint "s" (layer "F.Cu")
        (pad "1" thru_hole oval (at 0 0 90) (size 1.5 1.8) (drill oval 0.6 1.2) (layers "*.Cu")))"#;
        assert!(same_land_pattern(slot, slot_turned).unwrap(), "turned slot");
    }

    /// Anything that moves copper, reshapes it, or changes the hole is a different part.
    ///
    /// The direction that matters most: accepting one of these substitutes a component that does not
    /// fit the board.
    #[test]
    fn a_physical_difference_is_rejected() {
        let rounded = BASE.replace("smd rect", "smd roundrect").replace(
            r#"(layers "F.Cu" "F.Mask"))"#,
            r#"(layers "F.Cu" "F.Mask") (roundrect_rratio 0.25))"#,
        );
        let extra_pad = BASE.replace(
            r#"(pad "2""#,
            r#"(pad "3" smd rect (at 0 0) (size 1 1) (layers "F.Cu")) (pad "2""#,
        );
        for (case, source) in [
            ("moved pad", BASE.replace("(at 6.6 3.8)", "(at 6.7 3.8)")),
            (
                "resized pad",
                BASE.replace("(size 1.8 1.5)", "(size 1.9 1.5)"),
            ),
            (
                "through-hole, not SMD",
                BASE.replace("smd rect", "thru_hole rect"),
            ),
            (
                "oval, not rectangular",
                BASE.replace("smd rect", "smd oval"),
            ),
            ("other side of the board", BASE.replace("F.Cu", "B.Cu")),
            ("rounded corner", rounded),
            ("extra numbered pad", extra_pad),
            ("renumbered pad", BASE.replace(r#"(pad "2""#, r#"(pad "9""#)),
            (
                "odd rotation",
                BASE.replace("(at -6.6 -3.8)", "(at -6.6 -3.8 30)"),
            ),
        ] {
            assert!(!same_land_pattern(BASE, &source).unwrap(), "{case}");
        }

        // The hole is part of the part. Ignoring it was the one difference here that could produce a
        // physically wrong board rather than a missed substitution.
        let hole = r#"(footprint "t" (layer "F.Cu")
        (pad "1" thru_hole circle (at 0 0) (size 1.6 1.6) (drill 0.6) (layers "*.Cu")))"#;
        let smd = hole
            .replace("thru_hole circle", "smd circle")
            .replace("(drill 0.6) ", "");
        for (case, source) in [
            ("wider drill", hole.replace("(drill 0.6)", "(drill 1.1)")),
            (
                "slotted drill",
                hole.replace("(drill 0.6)", "(drill oval 0.6 1.2)"),
            ),
            (
                "offset hole",
                hole.replace("(drill 0.6)", "(drill 0.6 (offset -0.3 0))"),
            ),
            ("no hole at all", smd),
        ] {
            assert!(!same_land_pattern(hole, &source).unwrap(), "{case}");
        }

        // A chamfer is written on a `roundrect` with no radius, which the shape normalization would
        // otherwise reduce to a plain rectangle and lose.
        let square = r#"(footprint "q" (layer "F.Cu")
        (pad "1" smd roundrect (at 0 0) (size 1 1) (layers "F.Cu") (roundrect_rratio 0)))"#;
        let chamfered = square.replace(
            "(roundrect_rratio 0)",
            "(roundrect_rratio 0) (chamfer_ratio 0.3) (chamfer top_left)",
        );
        assert!(!same_land_pattern(square, &chamfered).unwrap(), "chamfer");
        assert!(
            !same_land_pattern(&chamfered, &chamfered.replace("top_left", "bottom_right")).unwrap(),
            "a different corner is a different outline"
        );

        // One number can name many pads — a split thermal pad, a via array. `Quectel_LG290P` carries
        // pad "25" fifty-five times, and collapsing them made it equal to a single-pad footprint.
        let array = r#"(footprint "a" (layer "F.Cu")
        (pad "9" smd rect (at 2 0) (size 1 1) (layers "F.Cu"))
        (pad "9" smd rect (at 3 0) (size 1 1) (layers "F.Cu")))"#;
        let single = r#"(footprint "b" (layer "F.Cu")
        (pad "9" smd rect (at 3 0) (size 1 1) (layers "F.Cu")))"#;
        let reordered = r#"(footprint "a" (layer "F.Cu")
        (pad "9" smd rect (at 3 0) (size 1 1) (layers "F.Cu"))
        (pad "9" smd rect (at 2 0) (size 1 1) (layers "F.Cu")))"#;
        assert!(
            !same_land_pattern(array, single).unwrap(),
            "pad multiplicity"
        );
        assert!(same_land_pattern(array, reordered).unwrap(), "pad order");
    }

    /// A chamfer and an offset hole point somewhere, so a turn moves them and the angle cannot be
    /// folded away. Folding it compared two physically different pads as equal.
    #[test]
    fn a_turn_is_not_folded_away_for_a_pad_that_points_somewhere() {
        for source in [
            r#"(footprint "c" (layer "F.Cu")
        (pad "1" smd roundrect (at 0 0) (size 1 0.6) (layers "F.Cu")
          (roundrect_rratio 0) (chamfer_ratio 0.3) (chamfer top_left)))"#,
            r#"(footprint "h" (layer "F.Cu")
        (pad "1" thru_hole rect (at 0 0) (size 1.3 0.9) (drill 0.6 (offset -0.3 0))
          (layers "*.Cu")))"#,
            r#"(footprint "e" (layer "F.Cu")
        (pad "1" thru_hole circle (at 0 0) (size 2 2) (drill 0.8 (offset 0.4 0))
          (layers "*.Cu")))"#,
        ] {
            let turned = source.replace("(at 0 0)", "(at 0 0 180)");
            assert!(
                !same_land_pattern(source, &turned).unwrap(),
                "a half turn moves the feature"
            );
            assert!(same_land_pattern(source, source).unwrap());
        }
    }

    /// Geometry this comparison cannot read must not be compared at all.
    ///
    /// A `custom` pad's outline lives in `primitives`; only its anchor size and the shape token would
    /// be compared, so two arbitrary shapes looked identical. A board-instance footprint folds its
    /// placement angle into every pad and has to be de-instanced first, as generation already does.
    #[test]
    fn geometry_that_cannot_be_read_falls_back_instead_of_guessing() {
        let custom = r#"(footprint "c" (layer "F.Cu")
        (pad "1" smd custom (at 0 0) (size 1 1) (layers "F.Cu")
          (options (clearance outline) (anchor rect))
          (primitives (gr_poly (pts (xy 0 0) (xy 5 0) (xy 5 5)) (width 0)))))"#;
        assert!(has_incomparable_pad(custom));
        assert!(
            component_footprint_source(&component_with_geometry(
                ImportFootprintGeometry::LibraryFile(custom.to_string())
            ))
            .is_none(),
            "a custom pad is offered as having no comparable geometry"
        );

        let library = r#"(footprint "p" (layer "F.Cu")
        (pad "1" smd rect (at 1 0) (size 1.8 1.5) (layers "F.Cu" "F.Mask")))"#;
        let placed = r#"(footprint "p" (layer "F.Cu") (at 50 40 90)
        (pad "1" smd rect (at 1 0 90) (size 1.8 1.5) (layers "F.Cu" "F.Mask")))"#;
        assert!(
            !same_land_pattern(library, placed).unwrap(),
            "raw board text carries the placement angle"
        );
        let component =
            component_with_geometry(ImportFootprintGeometry::BoardInstance(placed.to_string()));
        let de_instanced = component_footprint_source(&component)
            .expect("a board instance is offered in library form");
        assert!(
            same_land_pattern(library, &de_instanced).unwrap(),
            "de-instanced, it is the same land pattern"
        );

        // A footprint with no numbered pads cannot be compared, and says so rather than comparing
        // equal to another padless one.
        let padless = r#"(footprint "x" (fp_text reference "REF" (at 0 0)))"#;
        assert!(pad_lands(padless).is_err());
        assert!(same_land_pattern(padless, BASE).is_err());
    }

    fn component_with_geometry(geometry: ImportFootprintGeometry) -> ImportComponentData {
        ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: KiCadRefDes::from("U1".to_string()),
                value: None,
                footprint: Some("Lib:P".to_string()),
                sheetpath_names: None,
                unit_pcb_paths: vec![KiCadUuidPathKey::from_pcb_path("/u1").unwrap()],
            },
            schematic: None,
            layout: Some(ImportLayoutComponent {
                fpid: Some("Lib:P".to_string()),
                unresolved_footprint: None,
                uuid: None,
                layer: None,
                at: None,
                sheetname: None,
                sheetfile: None,
                attrs: Vec::new(),
                properties: BTreeMap::new(),
                pads: BTreeMap::new(),
                footprint_geometry: geometry,
            }),
        }
    }
}
