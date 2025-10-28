use super::resolved_feature::*;
use super::{BoardContext, Result};
use crate::{Ipc2581, Ipc2581Error, PadUse, StandardPrimitive, UserPrimitive, UserShapeType};
use std::collections::HashMap;

/// Stage 2: Padstack Expansion
///
/// Expands PadstackRef geometries into concrete shapes (Circle, Rectangle, etc.)
/// by looking up primitive definitions and applying transformations.
pub fn expand_padstacks(
    doc: &Ipc2581,
    context: &BoardContext,
    mut layer_resolutions: HashMap<String, LayerResolution>,
) -> Result<HashMap<String, LayerResolution>> {
    for (layer_name, layer_resolution) in layer_resolutions.iter_mut() {
        for feature in layer_resolution.features.iter_mut() {
            if let ResolvedGeometry::PadstackRef {
                padstack_name,
                center,
                rotation,
                inline_standard_primitive,
                inline_user_primitive,
                ..
            } = &feature.geometry
            {
                // Try to expand the padstack reference to concrete geometry
                if let Ok(expanded) = expand_padstack_ref(
                    doc,
                    context,
                    padstack_name,
                    layer_name,
                    *center,
                    *rotation,
                    inline_standard_primitive.as_deref(),
                    inline_user_primitive.as_deref(),
                ) {
                    feature.geometry = expanded;
                    feature.bbox = calculate_geometry_bbox(&feature.geometry);
                }
                // If expansion fails, leave as PadstackRef (will be caught in validation)
            }
        }

        // Recalculate layer bounding box
        layer_resolution.bbox = layer_resolution
            .features
            .iter()
            .fold(BoundingBox::empty(), |bbox, f| bbox.union(&f.bbox));
    }

    Ok(layer_resolutions)
}

/// Expand a padstack reference to concrete geometry
fn expand_padstack_ref(
    doc: &Ipc2581,
    context: &BoardContext,
    padstack_name: &str,
    layer_name: &str,
    center: Point,
    rotation: f64,
    inline_standard_prim: Option<&str>,
    inline_user_prim: Option<&str>,
) -> Result<ResolvedGeometry> {
    // 1. Try inline standard primitive override
    if let Some(prim_name) = inline_standard_prim {
        return lookup_and_expand_standard_primitive(doc, context, prim_name, center, rotation);
    }

    // 2. Try inline user primitive override
    if let Some(prim_name) = inline_user_prim {
        return lookup_and_expand_user_primitive(doc, context, prim_name, center, rotation);
    }

    // 3. Look up padstack definition
    let padstack_sym = doc
        .interner()
        .get(padstack_name)
        .ok_or(Ipc2581Error::MissingElement("Padstack name not in interner"))?;

    let padstack_def = context
        .padstack_defs
        .get(&padstack_sym)
        .ok_or(Ipc2581Error::MissingElement("PadStackDef not found"))?;

    // 4. Find pad definition for this layer
    let layer_sym = doc
        .interner()
        .get(layer_name)
        .ok_or(Ipc2581Error::MissingElement("Layer name not in interner"))?;

    let pad_def = padstack_def
        .pad_defs
        .iter()
        .find(|pd| pd.layer_ref == layer_sym && pd.pad_use == PadUse::Regular)
        .ok_or(Ipc2581Error::MissingElement("No PadDef for layer"))?;

    // 5. Expand primitive from pad definition
    if let Some(std_prim_ref) = pad_def.standard_primitive_ref {
        let prim = context
            .standard_primitives
            .get(&std_prim_ref)
            .ok_or(Ipc2581Error::MissingElement("StandardPrimitive not found"))?;
        return Ok(expand_primitive(prim, center, rotation));
    }

    if let Some(user_prim_ref) = pad_def.user_primitive_ref {
        let prim = context
            .user_primitives
            .get(&user_prim_ref)
            .ok_or(Ipc2581Error::MissingElement("UserPrimitive not found"))?;
        return Ok(expand_user_primitive(prim, center, rotation));
    }

    Err(Ipc2581Error::MissingElement(
        "No primitive reference in PadDef",
    ))
}

/// Look up and expand a standard primitive by name
fn lookup_and_expand_standard_primitive(
    doc: &Ipc2581,
    context: &BoardContext,
    prim_name: &str,
    center: Point,
    rotation: f64,
) -> Result<ResolvedGeometry> {
    let symbol = doc
        .interner()
        .get(prim_name)
        .ok_or(Ipc2581Error::MissingElement("Primitive name not in interner"))?;

    let primitive = context
        .standard_primitives
        .get(&symbol)
        .ok_or(Ipc2581Error::MissingElement("StandardPrimitive not found"))?;

    Ok(expand_primitive(primitive, center, rotation))
}

/// Look up and expand a user primitive by name
fn lookup_and_expand_user_primitive(
    doc: &Ipc2581,
    context: &BoardContext,
    prim_name: &str,
    center: Point,
    rotation: f64,
) -> Result<ResolvedGeometry> {
    let symbol = doc
        .interner()
        .get(prim_name)
        .ok_or(Ipc2581Error::MissingElement("Primitive name not in interner"))?;

    let primitive = context
        .user_primitives
        .get(&symbol)
        .ok_or(Ipc2581Error::MissingElement("UserPrimitive not found"))?;

    Ok(expand_user_primitive(primitive, center, rotation))
}

/// Convert a StandardPrimitive to ResolvedGeometry with transforms applied
fn expand_primitive(
    primitive: &StandardPrimitive,
    center: Point,
    rotation: f64,
) -> ResolvedGeometry {
    match primitive {
        StandardPrimitive::Circle(c) => ResolvedGeometry::Circle {
            center,
            diameter: c.diameter,
            filled: true,
        },

        StandardPrimitive::RectCenter(r) => {
            expand_rectangle_with_rotation(center, r.width, r.height, rotation)
        }
        StandardPrimitive::RectRound(r) => {
            expand_rectangle_with_rotation(center, r.width, r.height, rotation)
        }
        StandardPrimitive::RectCham(r) => {
            expand_rectangle_with_rotation(center, r.width, r.height, rotation)
        }
        StandardPrimitive::Oval(o) => {
            expand_rectangle_with_rotation(center, o.width, o.height, rotation)
        }
        StandardPrimitive::Ellipse(e) => {
            expand_rectangle_with_rotation(center, e.width, e.height, rotation)
        }

        StandardPrimitive::RectCorner(r) => {
            let width = (r.upper_right_x - r.lower_left_x).abs();
            let height = (r.upper_right_y - r.lower_left_y).abs();
            let rect_center = Point::new(
                center.x + (r.lower_left_x + r.upper_right_x) / 2.0,
                center.y + (r.lower_left_y + r.upper_right_y) / 2.0,
            );
            expand_rectangle_with_rotation(rect_center, width, height, rotation)
        }

        StandardPrimitive::Diamond(d) => {
            let points = [
                Point::new(0.0, -d.height / 2.0),
                Point::new(d.width / 2.0, 0.0),
                Point::new(0.0, d.height / 2.0),
                Point::new(-d.width / 2.0, 0.0),
            ];
            ResolvedGeometry::Polygon {
                points: transform_points(&points, center, rotation),
                has_curves: false,
            }
        }

        StandardPrimitive::Hexagon(h) => {
            let points = create_regular_polygon(6, h.point_to_point / 2.0);
            ResolvedGeometry::Polygon {
                points: transform_points(&points, center, rotation),
                has_curves: false,
            }
        }

        StandardPrimitive::Octagon(o) => {
            let points = create_regular_polygon(8, o.point_to_point / 2.0);
            ResolvedGeometry::Polygon {
                points: transform_points(&points, center, rotation),
                has_curves: false,
            }
        }

        StandardPrimitive::Triangle(t) => {
            let hw = t.base / 2.0;
            let hh = t.height / 2.0;
            let points = [
                Point::new(0.0, -hh),
                Point::new(hw, hh),
                Point::new(-hw, hh),
            ];
            ResolvedGeometry::Polygon {
                points: transform_points(&points, center, rotation),
                has_curves: false,
            }
        }

        // Complex shapes approximated as circles for Stage 2
        StandardPrimitive::Donut(d) => ResolvedGeometry::Circle {
            center,
            diameter: d.outer_diameter,
            filled: true,
        },
        StandardPrimitive::Thermal(t) => ResolvedGeometry::Circle {
            center,
            diameter: t.outer_diameter,
            filled: true,
        },
        StandardPrimitive::Butterfly(b) => ResolvedGeometry::Circle {
            center,
            diameter: b.size,
            filled: true,
        },
        StandardPrimitive::Moire(m) => ResolvedGeometry::Circle {
            center,
            diameter: m.diameter,
            filled: true,
        },

        StandardPrimitive::Contour(c) => {
            let mut points = vec![Point::new(c.polygon.begin.x, c.polygon.begin.y)];
            let mut has_curves = false;

            for step in &c.polygon.steps {
                match step {
                    crate::PolyStep::Segment(s) => points.push(Point::new(s.x, s.y)),
                    crate::PolyStep::Curve(c) => {
                        has_curves = true;
                        points.push(Point::new(c.x, c.y));
                    }
                }
            }

            ResolvedGeometry::Polygon {
                points: transform_points(&points, center, rotation),
                has_curves,
            }
        }
    }
}

/// Convert a UserPrimitive to ResolvedGeometry
fn expand_user_primitive(
    user_prim: &UserPrimitive,
    center: Point,
    rotation: f64,
) -> ResolvedGeometry {
    match user_prim {
        UserPrimitive::UserSpecial(special) => {
            if special.shapes.is_empty() {
                return ResolvedGeometry::Circle {
                    center,
                    diameter: 1.0,
                    filled: true,
                };
            }

            match &special.shapes[0].shape {
                UserShapeType::Circle(c) => ResolvedGeometry::Circle {
                    center,
                    diameter: c.diameter,
                    filled: true,
                },
                UserShapeType::RectCenter(r) => {
                    expand_rectangle_with_rotation(center, r.width, r.height, rotation)
                }
                UserShapeType::Oval(o) => {
                    expand_rectangle_with_rotation(center, o.width, o.height, rotation)
                }
                UserShapeType::Polygon(p) => {
                    let mut points = vec![Point::new(p.begin.x, p.begin.y)];
                    let mut has_curves = false;

                    for step in &p.steps {
                        match step {
                            crate::PolyStep::Segment(s) => points.push(Point::new(s.x, s.y)),
                            crate::PolyStep::Curve(c) => {
                                has_curves = true;
                                points.push(Point::new(c.x, c.y));
                            }
                        }
                    }

                    ResolvedGeometry::Polygon {
                        points: transform_points(&points, center, rotation),
                        has_curves,
                    }
                }
            }
        }
    }
}

/// Expand a rectangle with optional rotation
fn expand_rectangle_with_rotation(
    center: Point,
    width: f64,
    height: f64,
    rotation: f64,
) -> ResolvedGeometry {
    if rotation.abs() < 0.01 {
        ResolvedGeometry::Rectangle {
            center,
            width,
            height,
            filled: true,
        }
    } else {
        let hw = width / 2.0;
        let hh = height / 2.0;
        let corners = [
            Point::new(-hw, -hh),
            Point::new(hw, -hh),
            Point::new(hw, hh),
            Point::new(-hw, hh),
        ];
        ResolvedGeometry::Polygon {
            points: transform_points(&corners, center, rotation),
            has_curves: false,
        }
    }
}

/// Create a regular polygon with N sides
fn create_regular_polygon(sides: usize, radius: f64) -> Vec<Point> {
    let angle_step = 360.0 / sides as f64;
    (0..sides)
        .map(|i| {
            let angle = (i as f64 * angle_step).to_radians();
            Point::new(radius * angle.cos(), radius * angle.sin())
        })
        .collect()
}

/// Transform points by rotation and translation
fn transform_points(points: &[Point], center: Point, rotation: f64) -> Vec<Point> {
    points
        .iter()
        .map(|p| p.rotate(rotation).translate(center.x, center.y))
        .collect()
}

/// Calculate bounding box for a ResolvedGeometry
fn calculate_geometry_bbox(geometry: &ResolvedGeometry) -> BoundingBox {
    match geometry {
        ResolvedGeometry::Circle {
            center, diameter, ..
        } => {
            let radius = diameter / 2.0;
            BoundingBox {
                min_x: center.x - radius,
                min_y: center.y - radius,
                max_x: center.x + radius,
                max_y: center.y + radius,
            }
        }

        ResolvedGeometry::Rectangle {
            center,
            width,
            height,
            ..
        } => {
            let hw = width / 2.0;
            let hh = height / 2.0;
            BoundingBox {
                min_x: center.x - hw,
                min_y: center.y - hh,
                max_x: center.x + hw,
                max_y: center.y + hh,
            }
        }

        ResolvedGeometry::Polygon { points, .. } | ResolvedGeometry::Polyline { points, .. } => {
            if points.is_empty() {
                return BoundingBox::empty();
            }
            let mut bbox = BoundingBox::from_point(points[0]);
            for p in &points[1..] {
                bbox.expand_to_point(*p);
            }

            // Expand by line width if polyline
            if let ResolvedGeometry::Polyline { line_width, .. } = geometry {
                let half_width = line_width / 2.0;
                bbox = BoundingBox {
                    min_x: bbox.min_x - half_width,
                    min_y: bbox.min_y - half_width,
                    max_x: bbox.max_x + half_width,
                    max_y: bbox.max_y + half_width,
                };
            }

            bbox
        }

        ResolvedGeometry::PadstackRef { center, .. } => BoundingBox {
            min_x: center.x - 0.5,
            min_y: center.y - 0.5,
            max_x: center.x + 0.5,
            max_y: center.y + 0.5,
        },
    }
}
