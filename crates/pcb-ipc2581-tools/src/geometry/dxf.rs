use std::fmt::Write;

use pcb_ir::dialects::ipc::{Document, ProfileSet, profile_occurrences_for};
use pcb_ir::geom::{ContourBuf, GeometryAccuracy, Point, Segment};

use crate::utils::format::fmt_num;

const OUTLINE_LAYER: &str = "BOARD_OUTLINE";
const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, Copy)]
struct DxfVertex {
    x: f64,
    y: f64,
    bulge: f64,
}

pub fn render_profile_set_dxf<Symbol, LayerFunction>(
    doc: &Document<Symbol, LayerFunction>,
    profile_set: ProfileSet,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<String> {
    let mut dxf = String::new();
    write_header(&mut dxf);
    write_tables(&mut dxf);
    write_entities_start(&mut dxf);
    for occurrence in profile_occurrences_for(doc, profile_set) {
        write_path(
            &mut dxf,
            doc,
            occurrence.profile.outer_path,
            occurrence.transform,
            accuracy,
        )?;
        for cutout in occurrence.profile.cutouts.slice(&doc.profile_cutouts) {
            write_path(&mut dxf, doc, cutout.path, occurrence.transform, accuracy)?;
        }
    }
    write_footer(&mut dxf);
    Ok(dxf)
}

/// The file is plain R12: polylines with bulges need nothing newer, and R12
/// is the one version that requires no handles, owner references or object
/// tables. `$INSUNITS` postdates R12 but is how readers learn the file is in
/// millimetres; R12 readers skip header variables they do not know.
fn write_header(dxf: &mut String) {
    dxf.push_str("0\nSECTION\n2\nHEADER\n");
    dxf.push_str("9\n$ACADVER\n1\nAC1009\n");
    dxf.push_str("9\n$INSUNITS\n70\n4\n");
    dxf.push_str("0\nENDSEC\n");
}

fn write_tables(dxf: &mut String) {
    dxf.push_str("0\nSECTION\n2\nTABLES\n");
    dxf.push_str("0\nTABLE\n2\nLAYER\n70\n1\n");
    dxf.push_str("0\nLAYER\n2\nBOARD_OUTLINE\n70\n0\n62\n7\n6\nCONTINUOUS\n");
    dxf.push_str("0\nENDTAB\n0\nENDSEC\n");
}

fn write_entities_start(dxf: &mut String) {
    dxf.push_str("0\nSECTION\n2\nENTITIES\n");
}

fn write_footer(dxf: &mut String) {
    dxf.push_str("0\nENDSEC\n0\nEOF\n");
}

fn write_path<Symbol, LayerFunction>(
    dxf: &mut String,
    doc: &Document<Symbol, LayerFunction>,
    path_index: u32,
    transform: pcb_ir::geom::Affine2,
    accuracy: GeometryAccuracy,
) -> anyhow::Result<()> {
    for contour in doc.transformed_path_contours(path_index, transform) {
        let contour = contour.flattened_curves(accuracy)?;
        write_polyline(dxf, &contour_vertices(&contour));
    }
    Ok(())
}

/// A closed 2D polyline: flag 66 announces the vertices that follow, flag 70
/// closes the last vertex back to the first.
fn write_polyline(dxf: &mut String, vertices: &[DxfVertex]) {
    if vertices.len() < 2 {
        return;
    }

    writeln!(dxf, "0\nPOLYLINE\n8\n{OUTLINE_LAYER}\n62\n7\n66\n1\n70\n1").unwrap();
    for vertex in vertices {
        writeln!(
            dxf,
            "0\nVERTEX\n8\n{OUTLINE_LAYER}\n10\n{}\n20\n{}",
            fmt_num(vertex.x),
            fmt_num(vertex.y)
        )
        .unwrap();
        if vertex.bulge.abs() > EPSILON {
            writeln!(dxf, "42\n{}", fmt_num(vertex.bulge)).unwrap();
        }
    }
    writeln!(dxf, "0\nSEQEND\n8\n{OUTLINE_LAYER}").unwrap();
}

/// Polyline vertices of a contour whose curves were flattened to lines and
/// circular arcs. Each segment contributes its start vertex, carrying the
/// bulge of the arc that leaves it; the closed polyline supplies the return
/// to the first vertex.
fn contour_vertices(contour: &ContourBuf) -> Vec<DxfVertex> {
    let vertex = |point: Point, bulge: f64| DxfVertex {
        x: point.x,
        y: point.y,
        bulge,
    };
    let mut vertices = Vec::new();
    let mut end = None;
    for segment in contour.segments() {
        match segment {
            // A closing sliver between coincident points is not an edge.
            Segment::Line { start, end } if same_point(start, end) => continue,
            Segment::Line { start, .. } => vertices.push(vertex(start, 0.0)),
            Segment::Arc(arc) if arc.is_full_circle() => {
                // One bulge cannot span a full turn; split at the antipode.
                let bulge = if arc.clockwise { -1.0 } else { 1.0 };
                vertices.push(vertex(arc.start, bulge));
                vertices.push(vertex(arc.center * 2.0 - arc.start, bulge));
            }
            Segment::Arc(arc) => {
                let sweep = arc.sweep_radians();
                let sweep = if arc.clockwise { -sweep } else { sweep };
                vertices.push(vertex(arc.start, (sweep / 4.0).tan()));
            }
            Segment::Ellipse(_) => {
                unreachable!("curves are flattened before polyline conversion")
            }
        }
        end = Some(segment.end());
    }
    // An open contour stops short of its start; the polyline closes from there.
    if let (Some(first), Some(end)) = (vertices.first(), end)
        && !same_point(end, Point::new(first.x, first.y))
    {
        vertices.push(vertex(end, 0.0));
    }

    vertices
}

fn same_point(a: Point, b: Point) -> bool {
    (a.x - b.x).abs() <= EPSILON && (a.y - b.y).abs() <= EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::dialects::ipc::{StepProfile, StepProfileCutout};
    use pcb_ir::geom::BBox;
    use pcb_ir::geom::path::PathCmd;
    use pcb_ir::geom::{ContourBuf, Paint, Span};

    #[test]
    fn renders_profile_ir_as_mm_dxf_with_closed_outline_layer() {
        let doc = rect_profile_doc();

        let dxf = render_profile_set_dxf(
            &doc,
            ProfileSet::FabricationOutlines,
            GeometryAccuracy::default(),
        )
        .unwrap();

        assert!(dxf.contains("9\n$ACADVER\n1\nAC1009\n"));
        assert!(dxf.contains("9\n$INSUNITS\n70\n4\n"));
        assert!(dxf.contains("2\nBOARD_OUTLINE\n"));
        assert_eq!(
            dxf.matches("0\nPOLYLINE\n8\nBOARD_OUTLINE\n62\n7\n66\n1\n70\n1\n")
                .count(),
            2
        );
        assert_eq!(dxf.matches("0\nVERTEX\n").count(), 8);
        assert_eq!(dxf.matches("0\nSEQEND\n").count(), 2);
        // R12 entities carry no subclass markers.
        assert!(!dxf.contains("AcDb"));
    }

    #[test]
    fn closing_sliver_keeps_the_last_arc() {
        // The second arc ends a hair from the start, so the contour closes
        // with a sub-nanometre line that must not flatten the arc before it.
        let vertices = contour_vertices(&ContourBuf::new(vec![
            PathCmd::move_to(Point::new(1.0, 0.0)),
            PathCmd::arc_to(Point::new(-1.0, 0.0), Point::new(0.0, 0.0), false),
            PathCmd::arc_to(Point::new(1.0, 5e-10), Point::new(0.0, 0.0), false),
            PathCmd::close(),
        ]));

        assert_eq!(vertices.len(), 2);
        assert!((vertices[0].bulge - 1.0).abs() < 1e-9);
        assert!((vertices[1].bulge - 1.0).abs() < 1e-9);
    }

    #[test]
    fn full_circle_splits_into_two_half_turns() {
        let vertices = contour_vertices(&ContourBuf::new(vec![
            PathCmd::move_to(Point::new(1.0, 0.0)),
            PathCmd::arc_to(Point::new(1.0, 0.0), Point::new(0.0, 0.0), true),
            PathCmd::close(),
        ]));

        assert_eq!(vertices.len(), 2);
        assert_eq!(
            (vertices[0].x, vertices[0].y, vertices[0].bulge),
            (1.0, 0.0, -1.0)
        );
        assert_eq!(
            (vertices[1].x, vertices[1].y, vertices[1].bulge),
            (-1.0, 0.0, -1.0)
        );
    }

    #[test]
    fn preserves_profile_arcs_as_polyline_bulges() {
        let mut doc = Document::<u32, ()>::new();
        let path = doc.push_path(
            Paint::None,
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(1.0, 0.0)),
                PathCmd::arc_to(Point::new(-1.0, 0.0), Point::new(0.0, 0.0), false),
                PathCmd::arc_to(Point::new(1.0, 0.0), Point::new(0.0, 0.0), false),
                PathCmd::close(),
            ])],
        );
        doc.profiles.push(StepProfile {
            outer_path: path,
            cutouts: Span::EMPTY,
            bbox: BBox::empty(),
        });

        let dxf = render_profile_set_dxf(
            &doc,
            ProfileSet::FabricationOutlines,
            GeometryAccuracy::default(),
        )
        .unwrap();

        assert_eq!(dxf.matches("42\n1\n").count(), 2);
    }

    fn rect_profile_doc() -> Document<u32, ()> {
        let mut doc = Document::new();
        let outer_path = doc.push_path(
            Paint::None,
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 0.0)),
                PathCmd::line_to(Point::new(10.0, 5.0)),
                PathCmd::line_to(Point::new(0.0, 5.0)),
                PathCmd::close(),
            ])],
        );
        let cutout_path = doc.push_path(
            Paint::None,
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(4.0, 2.0)),
                PathCmd::line_to(Point::new(6.0, 2.0)),
                PathCmd::line_to(Point::new(6.0, 3.0)),
                PathCmd::line_to(Point::new(4.0, 3.0)),
                PathCmd::close(),
            ])],
        );
        doc.profile_cutouts.push(StepProfileCutout {
            path: cutout_path,
            bbox: BBox::empty(),
        });
        doc.profiles.push(StepProfile {
            outer_path,
            cutouts: Span::new(0, 1),
            bbox: BBox::empty(),
        });
        doc
    }
}
