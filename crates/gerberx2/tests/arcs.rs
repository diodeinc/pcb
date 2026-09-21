use gerberx2::{
    Contour, ContourSegment, GerberLayer, GerberX2, ObjectKind, Point, WriterAperture,
    WriterApertureTemplate, WriterObject, write_layer,
};
use pcb_ir::geom::{Affine2, Arc, GeometryAccuracy, Mirror, Point as GPoint, Resolution};

fn point(p: GPoint) -> Point {
    Point { x: p.x, y: p.y }
}

fn layer(arc: Arc, region: bool) -> GerberLayer {
    let start = point(arc.start);
    let end = point(arc.end);
    let center_offset = point(arc.center - arc.start);
    let clockwise = arc.clockwise;
    GerberLayer {
        apertures: vec![WriterAperture {
            code: 10,
            template: WriterApertureTemplate::Circle {
                diameter: 0.1,
                hole_diameter: None,
            },
            attributes: gerberx2::AttributeSets::EMPTY,
        }],
        objects: vec![WriterObject::dark(if region {
            let mut segments = vec![ContourSegment::Arc {
                start,
                end,
                center_offset,
                clockwise,
            }];
            if start != end {
                segments.push(ContourSegment::Line {
                    start: end,
                    end: start,
                });
            }
            ObjectKind::Region {
                contours: vec![Contour { segments }],
            }
        } else {
            ObjectKind::Arc {
                start,
                end,
                center_offset,
                clockwise,
                aperture: 10,
            }
        })],
        ..GerberLayer::default()
    }
}

fn parsed_arcs(parsed: &GerberX2) -> Vec<Arc> {
    let mut arcs = vec![];
    let mut push = |start: Point, end: Point, offset: Point, clockwise| {
        arcs.push(Arc::new(
            GPoint::new(start.x, start.y),
            GPoint::new(end.x, end.y),
            GPoint::new(start.x + offset.x, start.y + offset.y),
            clockwise,
        ));
    };
    for object in parsed.objects() {
        match &object.kind {
            ObjectKind::Arc {
                start,
                end,
                center_offset,
                clockwise,
                ..
            } => push(*start, *end, *center_offset, *clockwise),
            ObjectKind::Region { contours } => {
                for segment in contours.iter().flat_map(|c| &c.segments) {
                    if let ContourSegment::Arc {
                        start,
                        end,
                        center_offset,
                        clockwise,
                    } = segment
                    {
                        push(*start, *end, *center_offset, *clockwise);
                    }
                }
            }
            _ => panic!("expected arcs or regions"),
        }
    }
    arcs
}

#[test]
fn bounded_arcs_preserve_direction_sweep_continuity_and_region_area() {
    for degrees in [1.0_f64, 179.0, 180.0, 181.0, 359.0, 360.0] {
        for clockwise in [false, true] {
            for region in [false, true] {
                // Off-axis, translated center catches wrong I/J origins.
                let center = GPoint::new(12.345, -6.789);
                // Axis-aligned endpoints make the exact π boundary independent
                // of trigonometric roundoff. Other cases remain off-axis.
                let start_angle = if degrees == 180.0 { 0.0_f64 } else { 0.37_f64 };
                let end_angle =
                    start_angle + degrees.to_radians() * if clockwise { -1.0 } else { 1.0 };
                let start = center + GPoint::new(start_angle.cos(), start_angle.sin()) * 0.5;
                let end = if degrees == 360.0 {
                    start
                } else {
                    center + GPoint::new(end_angle.cos(), end_angle.sin()) * 0.5
                };
                let output =
                    write_layer(&layer(Arc::new(start, end, center, clockwise), region)).unwrap();
                gerber_parser::parse(std::io::BufReader::new(output.as_bytes())).unwrap();
                let parsed = GerberX2::parse(&output).unwrap();
                let arcs = parsed_arcs(&parsed);
                assert_eq!(arcs.len(), if degrees <= 180.0 { 1 } else { 2 });
                assert!(arcs[0].start.distance_to(start) < 1e-6);
                assert!(arcs.last().unwrap().end.distance_to(end) < 1e-6);
                for pair in arcs.windows(2) {
                    assert_eq!(pair[0].end, pair[1].start);
                }
                let mut sweep = 0.0;
                for arc in arcs {
                    assert_eq!(arc.clockwise, clockwise);
                    assert!(arc.center.distance_to(center) < 1.5e-6);
                    assert!((arc.radius() - 0.5).abs() < 1.5e-6);
                    // Coordinate quantization can move a semicircle slightly over π.
                    assert!(arc.sweep_radians() <= std::f64::consts::PI + 1e-5);
                    sweep += arc.sweep_radians();
                }
                assert!((sweep - degrees.to_radians()).abs() < 1e-5);
                if region {
                    let accuracy = GeometryAccuracy::new(0.0001).unwrap();
                    let geometry = gerberx2::geometry::extract_document(&parsed, accuracy).unwrap();
                    let actual = pcb_ir::dialects::artwork::compare::summarize(
                        &geometry,
                        Resolution::new(0.0, accuracy),
                    )
                    .unwrap()
                    .area_mm2;
                    let theta = degrees.to_radians();
                    let expected = 0.5 * 0.5_f64.powi(2) * (theta - theta.sin());
                    assert!(
                        (actual - expected).abs() < 0.0001,
                        "{degrees}: {actual} != {expected}"
                    );
                }
            }
        }
    }
}

#[test]
fn reported_near_circle_survives_mil_rounding_and_placements() {
    let source = Arc::new(
        GPoint::new(27.383048, 85.809924),
        GPoint::new(27.374322, 85.810000),
        GPoint::new(27.374331, 85.310000),
        true,
    );
    let mil_grid = |p: GPoint| ((p.x / 0.0254).round() as i64, (p.y / 0.0254).round() as i64);
    assert_ne!(source.start, source.end);
    assert_eq!(mil_grid(source.start), mil_grid(source.end));
    for transform in [
        Affine2::IDENTITY,
        Affine2::placement(GPoint::new(-7.25, 4.125), 37.0, Mirror::X, 1.7),
    ] {
        let ellipse = source.to_elliptical().transformed(transform);
        let arc = Arc::new(
            ellipse.start,
            ellipse.end,
            ellipse.center,
            ellipse.clockwise,
        );
        for region in [false, true] {
            let output = write_layer(&layer(arc, region)).unwrap();
            let parsed = GerberX2::parse(&output).unwrap();
            let arcs = parsed_arcs(&parsed);
            assert_eq!(arcs.len(), 2);
            for part in &arcs {
                assert_eq!(part.clockwise, arc.clockwise);
                assert_ne!(mil_grid(part.start), mil_grid(part.end));
                assert!(part.center.distance_to(arc.center) < 1.5e-6);
            }
            assert!(
                (arcs.iter().map(Arc::sweep_radians).sum::<f64>() - 359_f64.to_radians()).abs()
                    < 3e-5
            );
            if transform == Affine2::IDENTITY {
                // Midpoint from the negative normalized sum of the endpoint
                // unit vectors (the long arc's angular bisector).
                assert!(
                    output.contains("X27369977Y84810019I-8717J-499924D01*"),
                    "{output}"
                );
                assert!(
                    output.contains("X27374322Y85810000I4354J499981D01*"),
                    "{output}"
                );
            }
        }
    }
}
