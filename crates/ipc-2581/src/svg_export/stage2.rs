use super::resolved_feature::*;
use super::{BoardContext, Result};
use crate::{Ipc2581, Ipc2581Error, PadUse, StandardPrimitive, UserPrimitive, UserShapeType};
use std::collections::HashMap;

/// Transform parameters (rotation, mirror, scale)
#[derive(Debug, Clone, Copy)]
struct Transform {
    rotation: f64,
    mirror: bool,
    scale: f64,
}

/// Transform an arc segment by scale, mirror, rotation, and translation
fn transform_arc(
    arc: ArcSegment,
    center: Point,
    rotation: f64,
    mirror: bool,
    scale: f64,
) -> ArcSegment {
    ArcSegment {
        center: arc
            .center
            .scale(scale)
            .mirror_if(mirror)
            .rotate(rotation)
            .translate(center.x, center.y),
        clockwise: if mirror {
            !arc.clockwise
        } else {
            arc.clockwise
        },
    }
}

/// Orient rotation for mirrored shapes (mirror flips rotation direction)
fn orient_rotation(rotation: f64, mirror: bool) -> f64 {
    if mirror {
        -rotation
    } else {
        rotation
    }
}

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
                mirror,
                scale,
                inline_standard_primitive,
                inline_user_primitive,
                ..
            } = &feature.geometry
            {
                // Try to expand the padstack reference to concrete geometry
                let xform = Transform {
                    rotation: *rotation,
                    mirror: *mirror,
                    scale: *scale,
                };
                let expanded = expand_padstack_ref(
                    doc,
                    context,
                    padstack_name,
                    layer_name,
                    *center,
                    xform,
                    inline_standard_primitive.as_deref(),
                    inline_user_primitive.as_deref(),
                )?;
                feature.geometry = expanded;
                feature.bbox = calculate_geometry_bbox(&feature.geometry);
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
    xform: Transform,
    inline_standard_prim: Option<&str>,
    inline_user_prim: Option<&str>,
) -> Result<ResolvedGeometry> {
    // 1. Try inline standard primitive override
    if let Some(prim_name) = inline_standard_prim {
        return lookup_and_expand_standard_primitive(doc, context, prim_name, center, xform);
    }

    // 2. Try inline user primitive override
    if let Some(prim_name) = inline_user_prim {
        return lookup_and_expand_user_primitive(doc, context, prim_name, center, xform);
    }

    // 3. Look up padstack definition
    let padstack_sym = doc
        .interner()
        .get(padstack_name)
        .ok_or(Ipc2581Error::MissingElement(
            "Padstack name not in interner",
        ))?;

    let padstack_def = context
        .padstack_defs
        .get(&padstack_sym)
        .ok_or(Ipc2581Error::MissingElement("PadStackDef not found"))?;

    // 4. Find pad definition for this layer
    let layer_sym = doc
        .interner()
        .get(layer_name)
        .ok_or(Ipc2581Error::MissingElement("Layer name not in interner"))?;

    // Try to find REGULAR pad first, fall back to THERMAL if not found (plane layers)
    let pad_def = padstack_def
        .pad_defs
        .iter()
        .find(|pd| pd.layer_ref == layer_sym && pd.pad_use == PadUse::Regular)
        .or_else(|| {
            padstack_def
                .pad_defs
                .iter()
                .find(|pd| pd.layer_ref == layer_sym && pd.pad_use == PadUse::Thermal)
        })
        .ok_or(Ipc2581Error::MissingElement(
            "No PadDef for layer (tried REGULAR and THERMAL)",
        ))?;

    // 5. Expand primitive from pad definition
    if let Some(std_prim_ref) = pad_def.standard_primitive_ref {
        let prim = context
            .standard_primitives
            .get(&std_prim_ref)
            .ok_or(Ipc2581Error::MissingElement("StandardPrimitive not found"))?;
        return Ok(expand_primitive(prim, center, xform));
    }

    if let Some(user_prim_ref) = pad_def.user_primitive_ref {
        let prim = context
            .user_primitives
            .get(&user_prim_ref)
            .ok_or(Ipc2581Error::MissingElement("UserPrimitive not found"))?;
        return Ok(expand_user_primitive(prim, center, xform));
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
    xform: Transform,
) -> Result<ResolvedGeometry> {
    let symbol = doc
        .interner()
        .get(prim_name)
        .ok_or(Ipc2581Error::MissingElement(
            "Primitive name not in interner",
        ))?;

    let primitive = context
        .standard_primitives
        .get(&symbol)
        .ok_or(Ipc2581Error::MissingElement("StandardPrimitive not found"))?;

    Ok(expand_primitive(primitive, center, xform))
}

/// Look up and expand a user primitive by name
fn lookup_and_expand_user_primitive(
    doc: &Ipc2581,
    context: &BoardContext,
    prim_name: &str,
    center: Point,
    xform: Transform,
) -> Result<ResolvedGeometry> {
    let symbol = doc
        .interner()
        .get(prim_name)
        .ok_or(Ipc2581Error::MissingElement(
            "Primitive name not in interner",
        ))?;

    let primitive = context
        .user_primitives
        .get(&symbol)
        .ok_or(Ipc2581Error::MissingElement("UserPrimitive not found"))?;

    Ok(expand_user_primitive(primitive, center, xform))
}

/// Convert a StandardPrimitive to ResolvedGeometry with transforms applied
fn expand_primitive(
    primitive: &StandardPrimitive,
    center: Point,
    xform: Transform,
) -> ResolvedGeometry {
    let rotation = xform.rotation;
    let mirror = xform.mirror;
    let scale = xform.scale;
    match primitive {
        StandardPrimitive::Circle(c) => ResolvedGeometry::Circle {
            center,
            diameter: c.diameter * scale,
            filled: true,
        },

        StandardPrimitive::RectCenter(r) => {
            expand_rectangle_with_rotation(center, r.width, r.height, rotation, mirror, scale)
        }
        StandardPrimitive::RectRound(r) => {
            // When mirrored, left and right corners swap
            let corners = if mirror {
                [r.upper_left, r.upper_right, r.lower_left, r.lower_right]
            } else {
                [r.upper_right, r.upper_left, r.lower_right, r.lower_left]
            };
            ResolvedGeometry::RoundedRectangle {
                center,
                width: r.width * scale,
                height: r.height * scale,
                radius: r.radius * scale,
                corners,
                rotation: orient_rotation(rotation, mirror),
            }
        }
        StandardPrimitive::RectCham(r) => {
            // When mirrored, left and right corners swap
            let corners = if mirror {
                [r.upper_left, r.upper_right, r.lower_left, r.lower_right]
            } else {
                [r.upper_right, r.upper_left, r.lower_right, r.lower_left]
            };
            ResolvedGeometry::ChamferedRectangle {
                center,
                width: r.width * scale,
                height: r.height * scale,
                chamfer: r.chamfer * scale,
                corners,
                rotation: orient_rotation(rotation, mirror),
            }
        }
        StandardPrimitive::Oval(o) => ResolvedGeometry::Ellipse {
            center,
            width: o.width * scale,
            height: o.height * scale,
            rotation: orient_rotation(rotation, mirror),
        },
        StandardPrimitive::Ellipse(e) => ResolvedGeometry::Ellipse {
            center,
            width: e.width * scale,
            height: e.height * scale,
            rotation: orient_rotation(rotation, mirror),
        },

        StandardPrimitive::RectCorner(r) => {
            // Convert to 4 corners in local coordinates, then transform
            let corners = [
                Point::new(r.lower_left_x, r.lower_left_y),
                Point::new(r.upper_right_x, r.lower_left_y),
                Point::new(r.upper_right_x, r.upper_right_y),
                Point::new(r.lower_left_x, r.upper_right_y),
            ];
            let transformed_points = transform_points(&corners, center, rotation, mirror, scale);
            ResolvedGeometry::Polygon {
                points: transformed_points.clone(),
                arc_segments: vec![None; transformed_points.len()],
                cutouts: vec![],
                cutout_arcs: vec![],
            }
        }

        StandardPrimitive::Diamond(d) => {
            let points = [
                Point::new(0.0, -d.height / 2.0),
                Point::new(d.width / 2.0, 0.0),
                Point::new(0.0, d.height / 2.0),
                Point::new(-d.width / 2.0, 0.0),
            ];
            let transformed_points = transform_points(&points, center, rotation, mirror, scale);
            ResolvedGeometry::Polygon {
                points: transformed_points.clone(),
                arc_segments: vec![None; transformed_points.len()],
                cutouts: vec![],
                cutout_arcs: vec![],
            }
        }

        StandardPrimitive::Hexagon(h) => {
            let points = create_regular_polygon(6, h.point_to_point / 2.0);
            let transformed_points = transform_points(&points, center, rotation, mirror, scale);
            ResolvedGeometry::Polygon {
                points: transformed_points.clone(),
                arc_segments: vec![None; transformed_points.len()],
                cutouts: vec![],
                cutout_arcs: vec![],
            }
        }

        StandardPrimitive::Octagon(o) => {
            let points = create_regular_polygon(8, o.point_to_point / 2.0);
            let transformed_points = transform_points(&points, center, rotation, mirror, scale);
            ResolvedGeometry::Polygon {
                points: transformed_points.clone(),
                arc_segments: vec![None; transformed_points.len()],
                cutouts: vec![],
                cutout_arcs: vec![],
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
            let transformed_points = transform_points(&points, center, rotation, mirror, scale);
            ResolvedGeometry::Polygon {
                points: transformed_points.clone(),
                arc_segments: vec![None; transformed_points.len()],
                cutouts: vec![],
                cutout_arcs: vec![],
            }
        }

        // Complex shapes preserved as parametric geometries for Stage 3
        StandardPrimitive::Donut(d) => ResolvedGeometry::Donut {
            center,
            outer_diameter: d.outer_diameter * scale,
            inner_diameter: d.inner_diameter * scale,
        },
        StandardPrimitive::Thermal(t) => {
            // Calculate gap from spoke width if available, otherwise use a default proportion
            let gap = t.spoke_width.unwrap_or(t.outer_diameter * 0.15) * scale;
            ResolvedGeometry::Thermal {
                center,
                outer_diameter: t.outer_diameter * scale,
                inner_diameter: t.inner_diameter * scale,
                gap,
                spokes: t.spoke_count as u8,
                rotation: orient_rotation(rotation, mirror),
            }
        }
        StandardPrimitive::Butterfly(b) => ResolvedGeometry::Circle {
            center,
            diameter: b.size * scale,
            filled: true,
        },
        StandardPrimitive::Moire(m) => ResolvedGeometry::Circle {
            center,
            diameter: m.diameter * scale,
            filled: true,
        },

        StandardPrimitive::Contour(contour) => {
            // Process main polygon outline and preserve arc data
            let mut points = vec![Point::new(contour.polygon.begin.x, contour.polygon.begin.y)];
            let mut arc_segments = vec![None]; // First point has no arc

            for step in &contour.polygon.steps {
                match step {
                    crate::PolyStep::Segment(s) => {
                        points.push(Point::new(s.x, s.y));
                        arc_segments.push(None);
                    }
                    crate::PolyStep::Curve(c) => {
                        // Preserve arc data for Stage 3
                        points.push(Point::new(c.x, c.y));
                        arc_segments.push(Some(super::resolved_feature::ArcSegment {
                            center: Point::new(c.center_x, c.center_y),
                            clockwise: c.clockwise,
                        }));
                    }
                }
            }

            // Process cutouts (holes) and preserve arc data
            let mut cutouts = Vec::new();
            let mut cutout_arcs = Vec::new();
            for cutout_poly in &contour.cutouts {
                let mut cutout_points = vec![Point::new(cutout_poly.begin.x, cutout_poly.begin.y)];
                let mut cutout_arc_segments = vec![None];
                for step in &cutout_poly.steps {
                    match step {
                        crate::PolyStep::Segment(s) => {
                            cutout_points.push(Point::new(s.x, s.y));
                            cutout_arc_segments.push(None);
                        }
                        crate::PolyStep::Curve(c) => {
                            cutout_points.push(Point::new(c.x, c.y));
                            cutout_arc_segments.push(Some(super::resolved_feature::ArcSegment {
                                center: Point::new(c.center_x, c.center_y),
                                clockwise: c.clockwise,
                            }));
                        }
                    }
                }
                cutouts.push(transform_points(
                    &cutout_points,
                    center,
                    rotation,
                    mirror,
                    scale,
                ));
                // Transform arc centers as well
                let transformed_arc_segments: Vec<Option<ArcSegment>> = cutout_arc_segments
                    .into_iter()
                    .map(|arc_opt| {
                        arc_opt.map(|arc| transform_arc(arc, center, rotation, mirror, scale))
                    })
                    .collect();
                cutout_arcs.push(transformed_arc_segments);
            }

            // Transform main polygon arc centers
            let transformed_arc_segments: Vec<Option<ArcSegment>> = arc_segments
                .into_iter()
                .map(|arc_opt| {
                    arc_opt.map(|arc| transform_arc(arc, center, rotation, mirror, scale))
                })
                .collect();

            ResolvedGeometry::Polygon {
                points: transform_points(&points, center, rotation, mirror, scale),
                arc_segments: transformed_arc_segments,
                cutouts,
                cutout_arcs,
            }
        }
    }
}

/// Convert a UserPrimitive to ResolvedGeometry
fn expand_user_primitive(
    user_prim: &UserPrimitive,
    center: Point,
    xform: Transform,
) -> ResolvedGeometry {
    match user_prim {
        UserPrimitive::UserSpecial(special) => {
            if special.shapes.is_empty() {
                return ResolvedGeometry::Circle {
                    center,
                    diameter: 1.0 * xform.scale,
                    filled: true,
                };
            }

            // Process all shapes and return Group if multiple
            let geometries: Vec<ResolvedGeometry> = special
                .shapes
                .iter()
                .map(|user_shape| expand_user_shape(user_shape, center, xform))
                .collect();

            if geometries.len() == 1 {
                // Single shape - return directly
                geometries.into_iter().next().unwrap()
            } else {
                // Multiple shapes - wrap in Group
                ResolvedGeometry::Group { geometries }
            }
        }
    }
}

/// Convert a single UserShape to ResolvedGeometry
fn expand_user_shape(
    user_shape: &crate::UserShape,
    center: Point,
    xform: Transform,
) -> ResolvedGeometry {
    let rotation = xform.rotation;
    let mirror = xform.mirror;
    let scale = xform.scale;
    match &user_shape.shape {
        UserShapeType::Circle(c) => ResolvedGeometry::Circle {
            center,
            diameter: c.diameter * scale,
            filled: true,
        },
        UserShapeType::RectCenter(r) => {
            expand_rectangle_with_rotation(center, r.width, r.height, rotation, mirror, scale)
        }
        UserShapeType::Oval(o) => ResolvedGeometry::Ellipse {
            center,
            width: o.width * scale,
            height: o.height * scale,
            rotation: orient_rotation(rotation, mirror),
        },
        UserShapeType::Polygon(p) => {
            let mut points = vec![Point::new(p.begin.x, p.begin.y)];
            let mut arc_segments = vec![None];

            for step in &p.steps {
                match step {
                    crate::PolyStep::Segment(s) => {
                        points.push(Point::new(s.x, s.y));
                        arc_segments.push(None);
                    }
                    crate::PolyStep::Curve(c) => {
                        points.push(Point::new(c.x, c.y));
                        arc_segments.push(Some(super::resolved_feature::ArcSegment {
                            center: Point::new(c.center_x, c.center_y),
                            clockwise: c.clockwise,
                        }));
                    }
                }
            }

            // Transform arc centers
            let transformed_arc_segments: Vec<Option<ArcSegment>> = arc_segments
                .into_iter()
                .map(|arc_opt| {
                    arc_opt.map(|arc| transform_arc(arc, center, rotation, mirror, scale))
                })
                .collect();

            ResolvedGeometry::Polygon {
                points: transform_points(&points, center, rotation, mirror, scale),
                arc_segments: transformed_arc_segments,
                cutouts: vec![],
                cutout_arcs: vec![],
            }
        }
    }
}

/// Expand a rectangle with transforms applied
fn expand_rectangle_with_rotation(
    center: Point,
    width: f64,
    height: f64,
    rotation: f64,
    mirror: bool,
    scale: f64,
) -> ResolvedGeometry {
    let scaled_width = width * scale;
    let scaled_height = height * scale;

    if rotation.abs() < 0.01 && !mirror {
        ResolvedGeometry::Rectangle {
            center,
            width: scaled_width,
            height: scaled_height,
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
        let transformed_points = transform_points(&corners, center, rotation, mirror, scale);
        ResolvedGeometry::Polygon {
            points: transformed_points.clone(),
            arc_segments: vec![None; transformed_points.len()],
            cutouts: vec![],
            cutout_arcs: vec![],
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

/// Transform points by scale, mirror, rotation, and translation
fn transform_points(
    points: &[Point],
    center: Point,
    rotation: f64,
    mirror: bool,
    scale: f64,
) -> Vec<Point> {
    points
        .iter()
        .map(|p| {
            let mut pt = p.scale(scale);
            if mirror {
                pt = pt.mirror();
            }
            pt.rotate(rotation).translate(center.x, center.y)
        })
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
        }
        | ResolvedGeometry::RoundedRectangle {
            center,
            width,
            height,
            ..
        }
        | ResolvedGeometry::ChamferedRectangle {
            center,
            width,
            height,
            ..
        } => {
            // Conservative bbox (doesn't account for rotation, but safe approximation)
            let hw = width / 2.0;
            let hh = height / 2.0;
            let diagonal = (hw * hw + hh * hh).sqrt();
            BoundingBox {
                min_x: center.x - diagonal,
                min_y: center.y - diagonal,
                max_x: center.x + diagonal,
                max_y: center.y + diagonal,
            }
        }

        ResolvedGeometry::Polygon {
            points,
            arc_segments,
            ..
        } => {
            if points.is_empty() {
                return BoundingBox::empty();
            }
            let mut bbox = BoundingBox::from_point(points[0]);

            // Expand bbox for each point and arc bulge
            for (i, p) in points.iter().enumerate().skip(1) {
                bbox.expand_to_point(*p);

                // Account for arc bulge if this edge is curved
                if let Some(Some(arc)) = arc_segments.get(i) {
                    // Calculate arc radius from arc center to current point
                    let prev_point = points[i - 1];
                    let radius = ((arc.center.x - prev_point.x).powi(2)
                        + (arc.center.y - prev_point.y).powi(2))
                    .sqrt();

                    // Expand bbox to include arc center ± radius (conservative)
                    bbox.expand_to_point(Point::new(arc.center.x - radius, arc.center.y - radius));
                    bbox.expand_to_point(Point::new(arc.center.x + radius, arc.center.y + radius));
                }
            }

            bbox
        }

        ResolvedGeometry::Polyline {
            points, line_width, ..
        } => {
            if points.is_empty() {
                return BoundingBox::empty();
            }
            let mut bbox = BoundingBox::from_point(points[0]);
            for p in &points[1..] {
                bbox.expand_to_point(*p);
            }

            // Expand by line width
            let half_width = line_width / 2.0;
            BoundingBox {
                min_x: bbox.min_x - half_width,
                min_y: bbox.min_y - half_width,
                max_x: bbox.max_x + half_width,
                max_y: bbox.max_y + half_width,
            }
        }

        ResolvedGeometry::Ellipse {
            center,
            width,
            height,
            ..
        } => {
            // Conservative bbox (doesn't account for rotation, but safe approximation)
            let hw = width / 2.0;
            let hh = height / 2.0;
            let radius = hw.max(hh);
            BoundingBox {
                min_x: center.x - radius,
                min_y: center.y - radius,
                max_x: center.x + radius,
                max_y: center.y + radius,
            }
        }

        ResolvedGeometry::Donut {
            center,
            outer_diameter,
            ..
        } => {
            let radius = outer_diameter / 2.0;
            BoundingBox {
                min_x: center.x - radius,
                min_y: center.y - radius,
                max_x: center.x + radius,
                max_y: center.y + radius,
            }
        }

        ResolvedGeometry::Thermal {
            center,
            outer_diameter,
            ..
        } => {
            let radius = outer_diameter / 2.0;
            BoundingBox {
                min_x: center.x - radius,
                min_y: center.y - radius,
                max_x: center.x + radius,
                max_y: center.y + radius,
            }
        }

        ResolvedGeometry::PadstackRef { center, .. } => BoundingBox {
            min_x: center.x - 0.5,
            min_y: center.y - 0.5,
            max_x: center.x + 0.5,
            max_y: center.y + 0.5,
        },

        ResolvedGeometry::Group { geometries } => {
            if geometries.is_empty() {
                return BoundingBox::empty();
            }
            let mut bbox = calculate_geometry_bbox(&geometries[0]);
            for geom in &geometries[1..] {
                bbox = bbox.union(&calculate_geometry_bbox(geom));
            }
            bbox
        }
    }
}
