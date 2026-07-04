use std::fmt::Write;

use pcb_ir::dialects::ipc::{Document, ProfileSet, profile_occurrences_for};
use pcb_ir::geom::Point;

use crate::utils::format::fmt_num;
use pcb_ir::geom::path::{PathCmd, PathOp};

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
) -> String {
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
        );
        for cutout in occurrence.profile.cutouts.slice(&doc.profile_cutouts) {
            write_path(&mut dxf, doc, cutout.path, occurrence.transform);
        }
    }
    write_footer(&mut dxf);
    dxf
}

fn write_header(dxf: &mut String) {
    dxf.push_str("0\nSECTION\n2\nHEADER\n");
    dxf.push_str("9\n$ACADVER\n1\nAC1021\n");
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
) {
    for contour in doc.transformed_path_contours(path_index, transform) {
        write_polyline(dxf, &contour_vertices(&contour.cmds));
    }
}

fn write_polyline(dxf: &mut String, vertices: &[DxfVertex]) {
    if vertices.len() < 2 {
        return;
    }

    dxf.push_str("0\nLWPOLYLINE\n100\nAcDbEntity\n");
    writeln!(dxf, "8\n{OUTLINE_LAYER}").unwrap();
    dxf.push_str("62\n7\n100\nAcDbPolyline\n");
    writeln!(dxf, "90\n{}", vertices.len()).unwrap();
    dxf.push_str("70\n1\n");
    for vertex in vertices {
        writeln!(dxf, "10\n{}\n20\n{}", fmt_num(vertex.x), fmt_num(vertex.y)).unwrap();
        if vertex.bulge.abs() > EPSILON {
            writeln!(dxf, "42\n{}", fmt_num(vertex.bulge)).unwrap();
        }
    }
}

fn contour_vertices(cmds: &[PathCmd]) -> Vec<DxfVertex> {
    let mut first = None;
    let mut current = Point::default();
    let mut vertices = Vec::new();

    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                first = Some(cmd.p0);
                current = cmd.p0;
                vertices.push(DxfVertex {
                    x: cmd.p0.x,
                    y: cmd.p0.y,
                    bulge: 0.0,
                });
            }
            PathOp::LineTo => {
                current = cmd.p0;
                if let Some(first) = first {
                    vertices.last_mut().unwrap().bulge = 0.0;
                    push_endpoint(&mut vertices, current, first);
                }
            }
            PathOp::ArcTo => {
                let end = cmd.p0;
                let center = cmd.p1;
                if let Some(first) = first {
                    if same_point(current, end) && current.distance_to(center) > EPSILON {
                        let opposite = opposite_arc_point(current, center, cmd.clockwise);
                        vertices.last_mut().unwrap().bulge = half_circle_bulge(cmd.clockwise);
                        vertices.push(DxfVertex {
                            x: opposite.x,
                            y: opposite.y,
                            bulge: half_circle_bulge(cmd.clockwise),
                        });
                        push_endpoint(&mut vertices, end, first);
                    } else {
                        vertices.last_mut().unwrap().bulge =
                            arc_bulge(current, end, center, cmd.clockwise);
                        push_endpoint(&mut vertices, end, first);
                    }
                }
                current = end;
            }
            PathOp::CubicTo => {
                let start = current;
                for step in 1..=16 {
                    let end = cubic_point(start, cmd.p0, cmd.p1, cmd.p2, step as f64 / 16.0);
                    if let Some(first) = first {
                        vertices.last_mut().unwrap().bulge = 0.0;
                        push_endpoint(&mut vertices, end, first);
                    }
                    current = end;
                }
            }
            PathOp::Close => {}
        }
    }

    vertices
}

fn push_endpoint(vertices: &mut Vec<DxfVertex>, point: Point, first: Point) {
    if same_point(point, first) {
        return;
    }
    vertices.push(DxfVertex {
        x: point.x,
        y: point.y,
        bulge: 0.0,
    });
}

fn arc_bulge(start: Point, end: Point, center: Point, clockwise: bool) -> f64 {
    let start_angle = start.angle_from(center);
    let end_angle = end.angle_from(center);
    let ccw_sweep = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let signed_sweep = if clockwise {
        -(std::f64::consts::TAU - ccw_sweep)
    } else {
        ccw_sweep
    };
    (signed_sweep / 4.0).tan()
}

fn opposite_arc_point(start: Point, center: Point, clockwise: bool) -> Point {
    let radius = start.distance_to(center);
    let start_angle = start.angle_from(center);
    let angle = start_angle
        + if clockwise {
            -std::f64::consts::PI
        } else {
            std::f64::consts::PI
        };
    Point::new(
        center.x + radius * angle.cos(),
        center.y + radius * angle.sin(),
    )
}

fn half_circle_bulge(clockwise: bool) -> f64 {
    if clockwise { -1.0 } else { 1.0 }
}

fn cubic_point(start: Point, c1: Point, c2: Point, end: Point, t: f64) -> Point {
    let mt = 1.0 - t;
    Point::new(
        mt.powi(3) * start.x
            + 3.0 * mt.powi(2) * t * c1.x
            + 3.0 * mt * t.powi(2) * c2.x
            + t.powi(3) * end.x,
        mt.powi(3) * start.y
            + 3.0 * mt.powi(2) * t * c1.y
            + 3.0 * mt * t.powi(2) * c2.y
            + t.powi(3) * end.y,
    )
}

fn same_point(a: Point, b: Point) -> bool {
    (a.x - b.x).abs() <= EPSILON && (a.y - b.y).abs() <= EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::dialects::ipc::{StepProfile, StepProfileCutout};
    use pcb_ir::geom::BBox;
    use pcb_ir::geom::{ContourBuf, Paint, Span};

    #[test]
    fn renders_profile_ir_as_mm_dxf_with_closed_outline_layer() {
        let doc = rect_profile_doc();

        let dxf = render_profile_set_dxf(&doc, ProfileSet::FabricationOutlines);

        assert!(dxf.contains("9\n$INSUNITS\n70\n4\n"));
        assert!(dxf.contains("2\nBOARD_OUTLINE\n"));
        assert!(dxf.contains("0\nLWPOLYLINE\n"));
        assert!(dxf.contains("90\n4\n"));
        assert!(dxf.contains("70\n1\n"));
    }

    #[test]
    fn preserves_profile_arcs_as_lwpolyline_bulges() {
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

        let dxf = render_profile_set_dxf(&doc, ProfileSet::FabricationOutlines);

        assert!(dxf.contains("42\n1\n"));
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
