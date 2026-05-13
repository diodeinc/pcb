use std::fmt::Write;

use ipc2581::types::{PolyStep, Polygon, Profile};

const OUTLINE_LAYER: &str = "BOARD_OUTLINE";
const EPSILON: f64 = 1e-9;

#[derive(Debug, Clone, Copy)]
struct DxfVertex {
    x: f64,
    y: f64,
    bulge: f64,
}

/// Render an IPC-2581 board profile as an ASCII DXF outline in millimeters.
pub fn render_outline_dxf(profile: &Profile) -> String {
    let mut dxf = String::new();
    write_header(&mut dxf);
    write_tables(&mut dxf);
    write_entities_start(&mut dxf);
    write_polygon(&mut dxf, &profile.polygon);
    for cutout in &profile.cutouts {
        write_polygon(&mut dxf, cutout);
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

fn write_polygon(dxf: &mut String, polygon: &Polygon) {
    let vertices = polygon_vertices(polygon);
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

fn polygon_vertices(polygon: &Polygon) -> Vec<DxfVertex> {
    let first = point(polygon.begin);
    let mut current = first;
    let mut vertices = vec![DxfVertex {
        x: first.0,
        y: first.1,
        bulge: 0.0,
    }];

    for (index, step) in polygon.steps.iter().enumerate() {
        let is_last = index + 1 == polygon.steps.len();
        match step {
            PolyStep::Segment(segment) => {
                current = point(segment.point);
                vertices.last_mut().unwrap().bulge = 0.0;
                push_endpoint(&mut vertices, current, first, is_last);
            }
            PolyStep::Curve(curve) => {
                let end = point(curve.point);
                let center = point(curve.center);
                if same_point(current, end) && distance(current, center) > EPSILON {
                    let opposite = opposite_arc_point(current, center, curve.clockwise);
                    vertices.last_mut().unwrap().bulge = half_circle_bulge(curve.clockwise);
                    vertices.push(DxfVertex {
                        x: opposite.0,
                        y: opposite.1,
                        bulge: half_circle_bulge(curve.clockwise),
                    });
                    push_endpoint(&mut vertices, end, first, is_last);
                } else {
                    vertices.last_mut().unwrap().bulge =
                        arc_bulge(current, end, center, curve.clockwise);
                    push_endpoint(&mut vertices, end, first, is_last);
                }
                current = end;
            }
        }
    }

    vertices
}

fn push_endpoint(
    vertices: &mut Vec<DxfVertex>,
    point: (f64, f64),
    first: (f64, f64),
    is_last: bool,
) {
    if is_last && same_point(point, first) {
        return;
    }
    vertices.push(DxfVertex {
        x: point.0,
        y: point.1,
        bulge: 0.0,
    });
}

fn point(point: ipc2581::types::Point) -> (f64, f64) {
    (point.x, point.y)
}

fn arc_bulge(start: (f64, f64), end: (f64, f64), center: (f64, f64), clockwise: bool) -> f64 {
    let start_angle = (start.1 - center.1).atan2(start.0 - center.0);
    let end_angle = (end.1 - center.1).atan2(end.0 - center.0);
    let ccw_sweep = (end_angle - start_angle).rem_euclid(std::f64::consts::TAU);
    let signed_sweep = if clockwise {
        -(std::f64::consts::TAU - ccw_sweep)
    } else {
        ccw_sweep
    };
    (signed_sweep / 4.0).tan()
}

fn opposite_arc_point(start: (f64, f64), center: (f64, f64), clockwise: bool) -> (f64, f64) {
    let radius = distance(start, center);
    let start_angle = (start.1 - center.1).atan2(start.0 - center.0);
    let angle = start_angle
        + if clockwise {
            -std::f64::consts::PI
        } else {
            std::f64::consts::PI
        };
    (
        center.0 + radius * angle.cos(),
        center.1 + radius * angle.sin(),
    )
}

fn half_circle_bulge(clockwise: bool) -> f64 {
    if clockwise { -1.0 } else { 1.0 }
}

fn distance(a: (f64, f64), b: (f64, f64)) -> f64 {
    ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
}

fn same_point(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() <= EPSILON && (a.1 - b.1).abs() <= EPSILON
}

fn fmt_num(value: f64) -> String {
    if value.abs() < EPSILON {
        return "0".to_string();
    }
    let mut s = format!("{value:.6}");
    while s.contains('.') && s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipc2581::types::{Point, PolyStepCurve, PolyStepSegment};

    #[test]
    fn renders_profile_as_mm_dxf_with_closed_outline_layer() {
        let dxf = render_outline_dxf(&Profile {
            polygon: rect_polygon(),
            cutouts: Vec::new(),
        });

        assert!(dxf.contains("9\n$INSUNITS\n70\n4\n"));
        assert!(dxf.contains("2\nBOARD_OUTLINE\n"));
        assert!(dxf.contains("0\nLWPOLYLINE\n"));
        assert!(dxf.contains("90\n4\n"));
        assert!(dxf.contains("70\n1\n"));
    }

    #[test]
    fn preserves_profile_arcs_as_lwpolyline_bulges() {
        let dxf = render_outline_dxf(&Profile {
            polygon: Polygon {
                begin: Point { x: 1.0, y: 0.0 },
                steps: vec![
                    PolyStep::Curve(PolyStepCurve {
                        point: Point { x: -1.0, y: 0.0 },
                        center: Point { x: 0.0, y: 0.0 },
                        clockwise: false,
                    }),
                    PolyStep::Curve(PolyStepCurve {
                        point: Point { x: 1.0, y: 0.0 },
                        center: Point { x: 0.0, y: 0.0 },
                        clockwise: false,
                    }),
                ],
            },
            cutouts: Vec::new(),
        });

        assert!(dxf.contains("42\n1\n"));
    }

    fn rect_polygon() -> Polygon {
        Polygon {
            begin: Point { x: 0.0, y: 0.0 },
            steps: vec![
                PolyStep::Segment(PolyStepSegment {
                    point: Point { x: 10.0, y: 0.0 },
                }),
                PolyStep::Segment(PolyStepSegment {
                    point: Point { x: 10.0, y: 5.0 },
                }),
                PolyStep::Segment(PolyStepSegment {
                    point: Point { x: 0.0, y: 5.0 },
                }),
                PolyStep::Segment(PolyStepSegment {
                    point: Point { x: 0.0, y: 0.0 },
                }),
            ],
        }
    }
}
