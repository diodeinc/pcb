//! Castellated edges: plated half-holes the board outline cuts through.

use ipc2581::types::LayerFunction;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::geom::{ContourSet, Point, Resolution};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId};
use serde::Serialize;

use super::drills::{DrillHoleType, drill_hole};

/// One plated hole on the edge may be a stray, such as a mounting hole.
/// Diode's wire-pad castellation footprint has one half-hole per pad.
pub(crate) const MIN_CASTELLATED_HOLES: usize = 2;

/// Whether a plated hole whose centre is `edge_distance_mm` from the board
/// outline is a half-hole the outline cuts through.
pub(crate) fn on_edge(edge_distance_mm: f64, diameter_mm: f64) -> bool {
    edge_distance_mm.abs() <= diameter_mm / 4.0
}

#[derive(Debug, Clone, Serialize)]
pub struct Castellations {
    pub castellated: bool,
    /// Plated half-holes, castellated or not.
    pub hole_count: usize,
    /// JLCPCB's castellation rules; null unless castellated.
    pub jlcpcb: Option<JlcCastellationCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct JlcCastellationCheck {
    pub min_hole_mm: f64,
    /// Hole edge to hole edge.
    pub min_hole_spacing_mm: f64,
    /// Hole edge to outline corner; null when the outline has none.
    pub min_corner_distance_mm: Option<f64>,
    pub violations: Vec<String>,
}

/// Plated holes, not vias, whose centre lies within a quarter of their
/// diameter of the canonical board's outline.
pub fn castellations(imported: &ImportedDesign) -> anyhow::Result<Castellations> {
    let substrate = crate::commands::board_array::eligibility::board_substrate(
        imported,
        Resolution::default().strict(),
    )?;
    let outline = substrate.prepare_query();
    let mut holes: Vec<(Point, f64)> = Vec::new();
    for (index, layer) in imported.layer_definitions.iter().enumerate() {
        if layer.layer_function != LayerFunction::Drill {
            continue;
        }
        let doc = imported.materialize_layer(LayerId(index as u32), ArtworkScope::Board)?;
        for feature in &doc.features {
            if let Some((diameter, DrillHoleType::Plated)) = drill_hole(feature)
                && let Some(distance) = outline.signed_distance(feature.center)
                && on_edge(distance.mm, diameter)
            {
                holes.push((feature.center, diameter));
            }
        }
    }

    let castellated = holes.len() >= MIN_CASTELLATED_HOLES;
    let jlcpcb = castellated.then(|| jlc_check(&holes, &substrate));
    Ok(Castellations {
        castellated,
        hole_count: holes.len(),
        jlcpcb,
    })
}

fn jlc_check(holes: &[(Point, f64)], substrate: &ContourSet) -> JlcCastellationCheck {
    let um = |mm: f64| (mm * 1000.0).round() / 1000.0;
    let min_hole_mm = holes.iter().map(|h| h.1).fold(f64::INFINITY, f64::min);
    let min_hole_spacing_mm = um(holes
        .iter()
        .enumerate()
        .flat_map(|(i, a)| {
            holes[i + 1..]
                .iter()
                .map(move |b| a.0.distance_to(b.0) - (a.1 + b.1) / 2.0)
        })
        .fold(f64::INFINITY, f64::min));
    let corners = corners(substrate);
    let min_corner_distance_mm = holes
        .iter()
        .flat_map(|h| corners.iter().map(|c| h.0.distance_to(*c) - h.1 / 2.0))
        .reduce(f64::min)
        .map(um);
    let bbox = substrate.bbox();
    let mut violations = Vec::new();
    if min_hole_mm < 0.6 {
        violations.push(format!(
            "a castellated hole is {min_hole_mm} mm; JLCPCB needs 0.6 mm"
        ));
    }
    if min_hole_spacing_mm < 0.6 {
        violations.push(format!(
            "castellated holes are {min_hole_spacing_mm} mm apart, edge to edge; JLCPCB needs 0.6 mm"
        ));
    }
    if let Some(distance) = min_corner_distance_mm.filter(|&d| d < 3.0) {
        violations.push(format!(
            "a castellated hole is {distance} mm from a board corner; JLCPCB needs 3 mm"
        ));
    }
    if bbox.width().min(bbox.height()) < 10.0 {
        violations.push("the board is under 10 × 10 mm, JLCPCB's castellation minimum".into());
    }
    JlcCastellationCheck {
        min_hole_mm,
        min_hole_spacing_mm,
        min_corner_distance_mm,
        violations,
    }
}

/// Outline vertices where the outline turns by 30° or more; the chords of a
/// flattened arc turn by less.
fn corners(substrate: &ContourSet) -> Vec<Point> {
    let mut corners = Vec::new();
    for ring in substrate.rings() {
        let point = |i: usize| {
            let [x, y] = ring[i % ring.len()];
            Point::new(x, y)
        };
        for i in 0..ring.len() {
            let (u, v) = (point(i + 1) - point(i), point(i + 2) - point(i + 1));
            let turn = (u.x * v.y - u.y * v.x).atan2(u.x * v.x + u.y * v.y).abs();
            if turn >= 30f64.to_radians() {
                corners.push(point(i + 1));
            }
        }
    }
    corners
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20 × 15 mm board whose right side is a semicircle around (20, 7.5).
    fn board(holes: &[(f64, f64, f64, &str)]) -> Castellations {
        let holes = holes
            .iter()
            .map(|(x, y, d, plating)| {
                format!(
                    r#"<Set><Hole name="h" diameter="{d}" platingStatus="{plating}" plusTol="0" minusTol="0" x="{x}" y="{y}"/></Set>"#
                )
            })
            .collect::<String>();
        let ipc = ipc2581::Ipc2581::parse(&format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/></Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE"/>
    <Step name="board" type="BOARD">
      <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="20" y="0"/>
        <PolyStepCurve x="20" y="15" centerX="20" centerY="7.5" clockwise="false"/>
        <PolyStepSegment x="0" y="15"/><PolyStepSegment x="0" y="0"/></Polygon></Profile>
      <LayerFeature layerRef="Drill">{holes}</LayerFeature>
    </Step>
  </CadData></Ecad></IPC-2581>"#
        ))
        .unwrap();
        castellations(&pcb_ir::import::ipc2581::import_design(&ipc, Resolution::default()).unwrap())
            .unwrap()
    }

    #[test]
    fn plated_holes_the_outline_cuts_are_castellations() {
        // Vias, a hole inside the edge and one on the arc's chord don't count.
        let ignored = [
            (0.0, 5.0, 0.3, "VIA"),
            (0.0, 6.0, 0.3, "VIA"),
            (12.0, 0.5, 0.6, "PLATED"),
            (20.0, 7.5, 0.8, "PLATED"),
        ];
        let one = board(&[&ignored[..], &[(5.0, 0.0, 0.6, "PLATED")]].concat());
        assert!(!one.castellated);
        assert_eq!(one.hole_count, 1);

        // Two on the bottom edge, 5 mm from the corner at (0, 0), and two on
        // the arc 10° apart: 1.307 - 0.8 mm edge to edge.
        let on_arc = |degrees: f64| {
            let (sin, cos) = degrees.to_radians().sin_cos();
            (20.0 + 7.5 * cos, 7.5 + 7.5 * sin, 0.8, "PLATED")
        };
        let found = board(
            &[
                &ignored[..],
                &[
                    (5.0, 0.0, 0.6, "PLATED"),
                    (6.27, 0.0, 0.6, "PLATED"),
                    on_arc(0.0),
                    on_arc(10.0),
                ],
            ]
            .concat(),
        );
        assert!(found.castellated);
        assert_eq!(found.hole_count, 4);
        let jlc = found.jlcpcb.unwrap();
        assert_eq!(jlc.min_hole_spacing_mm, 0.507);
        assert_eq!(jlc.min_corner_distance_mm, Some(4.7));
        assert_eq!(jlc.violations.len(), 1, "{:?}", jlc.violations);
    }
}
