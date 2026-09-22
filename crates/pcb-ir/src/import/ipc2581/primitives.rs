//! Lowering of IPC placements, standard and user primitives, and their
//! styles into arena paths.

use super::*;

#[derive(Debug, Clone, Copy)]
pub(super) struct IpcPlacement {
    pub(super) center: Point,
    pub(super) xform: Xform,
    pub(super) transform: Affine2,
}

pub(super) fn ipc_placement(location: Point, xform: Option<Xform>) -> IpcPlacement {
    let xform = xform.unwrap_or_default();
    let mut transform = Affine2::placement(location, xform.rotation, Mirror::NONE, xform.scale);
    // IPC-2581C 3.3 mirrors X after rotation, before board-space translation.
    if xform.mirror {
        transform.m00 = -transform.m00;
        transform.m01 = -transform.m01;
    }
    let center = transform.transform_point(Point::new(xform.x_offset, xform.y_offset));
    transform.m02 = center.x;
    transform.m12 = center.y;

    IpcPlacement {
        center,
        xform,
        transform,
    }
}

pub(super) fn apply_ipc_placement(feature: &mut GeometryFeature, placement: IpcPlacement) {
    feature.transform = placement.transform;
    feature.center = placement.center;
}

/// Lower one member of the IPC-2581C `Feature` substitution group into paths
/// and report whether it is a VOID, with the dictionary entry it came from.
/// Warns and returns `None` when the shape cannot be drawn.
pub(super) fn lower_feature_shape(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    shape: &FeatureShape,
    transform: Affine2,
) -> Result<Option<(bool, Option<PrimitiveRef>)>> {
    Ok(Some(match shape {
        FeatureShape::StandardPrimitive(primitive) => (
            lower_standard_primitive(context, doc, primitive, transform)?,
            None,
        ),
        FeatureShape::StandardPrimitiveRef(id) => {
            let Some(primitive) = context.standard_primitives.get(id).copied() else {
                doc.warn(format!(
                    "Skipping feature because standard primitive '{}' is missing",
                    context.strings.resolve(*id)
                ));
                return Ok(None);
            };
            (
                lower_standard_primitive(context, doc, primitive, transform)?,
                Some(PrimitiveRef::Standard(*id)),
            )
        }
        FeatureShape::UserPrimitive(primitive) => {
            lower_user_primitive(context, doc, primitive, transform)?;
            (false, None)
        }
        FeatureShape::UserPrimitiveRef(id) => {
            let Some(primitive) = context.user_primitives.get(id).copied() else {
                doc.warn(format!(
                    "Skipping feature because user primitive '{}' is missing",
                    context.strings.resolve(*id)
                ));
                return Ok(None);
            };
            lower_user_primitive(context, doc, primitive, transform)?;
            (false, Some(PrimitiveRef::User(*id)))
        }
        FeatureShape::UserShape(shape) => {
            let primitive_start = doc.arena.paths.len();
            lower_user_shape(context, doc, shape, transform, primitive_start)?;
            (false, None)
        }
        FeatureShape::Text(_) | FeatureShape::Outline(_) => {
            doc.warn("Skipping feature whose shape is text or a package outline");
            return Ok(None);
        }
    }))
}

/// The line description an outline names: the dictionary entry behind its
/// `LineDescRef`, else its inline `LineDesc`. A reference the dictionary
/// lacks is reported; naming none is legitimate for a hollow outline, which
/// then has nothing to draw (KiCad writes zero-width outlines that way).
pub(super) fn resolve_line_desc(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    what: &str,
    reference: Option<Symbol>,
    inline: Option<ipc2581::types::LineDesc>,
) -> Option<ipc2581::types::LineDesc> {
    let Some(reference) = reference else {
        return inline;
    };
    let resolved = context.line_descs.get(&reference).copied();
    if resolved.is_none() {
        doc.warn(format!(
            "Not drawing {what}: LineDesc '{}' is missing",
            context.strings.resolve(reference)
        ));
    }
    resolved
}

/// [`resolve_line_desc`] for geometry that is nothing but its stroke, where
/// naming no description leaves no width to draw and is reported rather than
/// invented.
pub(super) fn require_line_desc(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    what: &str,
    reference: Option<Symbol>,
    inline: Option<ipc2581::types::LineDesc>,
) -> Option<ipc2581::types::LineDesc> {
    if reference.is_none() && inline.is_none() {
        doc.warn(format!("Not drawing {what}: it has no line description"));
    }
    resolve_line_desc(context, doc, what, reference, inline)
}

/// Paint for a line description whose geometry is placed at `scale`.
pub(super) fn stroke_paint(line_desc: ipc2581::types::LineDesc, scale: f64) -> Paint {
    let mut stroke = StrokeStyle::new(
        line_desc.line_width * scale,
        map_line_cap(line_desc.line_end),
    );
    stroke.pattern = map_line_pattern(line_desc.line_property);
    Paint::Stroke(stroke)
}

/// Uniform scale of a placement, as placement-group expansion measures it.
pub(super) fn placement_scale(transform: Affine2) -> f64 {
    transform.m00.hypot(transform.m10)
}

/// Lower a standard primitive into paths under `transform` and report
/// whether it is a VOID, which clears instead of painting.
pub(super) fn lower_standard_primitive(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    primitive: &StandardPrimitive,
    transform: Affine2,
) -> Result<bool> {
    let fill = primitive_fill_property(context, primitive);
    let void = fill == Some(FillProperty::Void);
    if standard_primitive_has_no_area(primitive) {
        return Ok(void);
    }
    warn_patterned_fill(doc, fill);

    let path_start = doc.arena.paths.len() as u32;
    let outline = match primitive {
        StandardPrimitive::Circle(circle) => shapes::circle(circle.shape.diameter),
        StandardPrimitive::Ellipse(ellipse) => {
            shapes::ellipse(ellipse.shape.size.width, ellipse.shape.size.height)
        }
        StandardPrimitive::Oval(oval) => {
            shapes::obround(oval.shape.size.width, oval.shape.size.height)
        }
        StandardPrimitive::RectCenter(rect) => {
            shapes::rect(rect.shape.size.width, rect.shape.size.height)
        }
        StandardPrimitive::RectCorner(rect) => {
            let (min, max) = (rect.shape.lower_left, rect.shape.upper_right);
            shapes::closed_polygon(vec![
                Point::new(min.x, min.y),
                Point::new(max.x, min.y),
                Point::new(max.x, max.y),
                Point::new(min.x, max.y),
            ])
        }
        StandardPrimitive::Diamond(diamond) => {
            let hw = diamond.shape.size.width / 2.0;
            let hh = diamond.shape.size.height / 2.0;
            shapes::closed_polygon(vec![
                Point::new(0.0, -hh),
                Point::new(hw, 0.0),
                Point::new(0.0, hh),
                Point::new(-hw, 0.0),
            ])
        }
        StandardPrimitive::Hexagon(hexagon) => {
            shapes::regular_polygon(hexagon.shape.point_to_point, 6, -90.0)
        }
        StandardPrimitive::Octagon(octagon) => {
            shapes::regular_polygon(octagon.shape.point_to_point, 8, -90.0)
        }
        StandardPrimitive::Triangle(triangle) => {
            let hw = triangle.shape.base / 2.0;
            let hh = triangle.shape.height / 2.0;
            shapes::closed_polygon(vec![
                Point::new(0.0, -hh),
                Point::new(hw, hh),
                Point::new(-hw, hh),
            ])
        }
        StandardPrimitive::RectRound(rect) => rect_round_outline(&rect.shape),
        StandardPrimitive::RectCham(rect) => shapes::chamfered_rect(
            rect.shape.size.width,
            rect.shape.size.height,
            rect.shape.chamfer,
            [
                rect.shape.upper_right,
                rect.shape.lower_right,
                rect.shape.lower_left,
                rect.shape.upper_left,
            ],
        ),
        StandardPrimitive::Donut(donut) => {
            push_ring_path(
                doc,
                transform,
                donut.shape.shape,
                donut.shape.outer_diameter,
                donut.shape.inner_diameter,
            );
            None
        }
        StandardPrimitive::Thermal(thermal) => {
            push_thermal_path(doc, transform, &thermal.shape, context.resolution)?;
            None
        }
        StandardPrimitive::Contour(contour) => {
            push_outline_path(doc, &contour.polygon, &contour.cutouts, transform);
            None
        }
        StandardPrimitive::Butterfly(butterfly) => {
            push_butterfly_path(doc, transform, butterfly.shape.shape, butterfly.shape.size);
            None
        }
        StandardPrimitive::Moire(moire) => {
            push_moire_path(doc, transform, moire);
            None
        }
    };
    push_filled_shape(doc, transform, outline);

    if fill == Some(FillProperty::Hollow) {
        let style = primitive_style(primitive);
        let line_desc = resolve_line_desc(
            context,
            doc,
            "hollow primitive",
            style.line_desc_ref,
            style.line_desc,
        );
        paint_paths(doc, path_start, line_desc, transform);
    }
    Ok(void)
}

pub(super) fn standard_primitive_has_no_area(primitive: &StandardPrimitive) -> bool {
    match primitive {
        StandardPrimitive::Circle(circle) => circle.shape.diameter <= 0.0,
        StandardPrimitive::Ellipse(ellipse) => {
            ellipse.shape.size.width <= 0.0 || ellipse.shape.size.height <= 0.0
        }
        StandardPrimitive::Oval(oval) => {
            oval.shape.size.width <= 0.0 || oval.shape.size.height <= 0.0
        }
        StandardPrimitive::RectCenter(rect) => {
            rect.shape.size.width <= 0.0 || rect.shape.size.height <= 0.0
        }
        StandardPrimitive::RectCorner(rect) => {
            rect.shape.upper_right.x <= rect.shape.lower_left.x
                || rect.shape.upper_right.y <= rect.shape.lower_left.y
        }
        StandardPrimitive::RectRound(rect) => {
            rect.shape.size.width <= 0.0 || rect.shape.size.height <= 0.0
        }
        StandardPrimitive::RectCham(rect) => {
            rect.shape.size.width <= 0.0 || rect.shape.size.height <= 0.0
        }
        StandardPrimitive::Diamond(diamond) => {
            diamond.shape.size.width <= 0.0 || diamond.shape.size.height <= 0.0
        }
        StandardPrimitive::Hexagon(hexagon) => hexagon.shape.point_to_point <= 0.0,
        StandardPrimitive::Octagon(octagon) => octagon.shape.point_to_point <= 0.0,
        StandardPrimitive::Triangle(triangle) => {
            triangle.shape.base <= 0.0 || triangle.shape.height <= 0.0
        }
        StandardPrimitive::Donut(donut) => {
            donut.shape.outer_diameter <= 0.0
                || donut.shape.inner_diameter >= donut.shape.outer_diameter
        }
        StandardPrimitive::Thermal(thermal) => {
            thermal.shape.outer_diameter <= 0.0
                || thermal.shape.inner_diameter >= thermal.shape.outer_diameter
        }
        StandardPrimitive::Butterfly(butterfly) => butterfly.shape.size <= 0.0,
        StandardPrimitive::Contour(_) | StandardPrimitive::Moire(_) => false,
    }
}

pub(super) fn lower_user_primitive(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    primitive: &UserPrimitive,
    transform: Affine2,
) -> Result<()> {
    match primitive {
        UserPrimitive::UserSpecial(user_special) => {
            let primitive_start = doc.arena.paths.len();
            // IPC-2581C §3.5.11.2: UserSpecial combines independent shapes.
            // A Contour's Polygon and Cutouts stay together (§3.5.9.3); sibling
            // contours are additive, including KiCad zone fills and text islands.
            for shape in &user_special.shapes {
                lower_user_shape(context, doc, shape, transform, primitive_start)?;
            }
            Ok(())
        }
    }
}

/// Lower one shape of a user primitive whose paths start at
/// `primitive_start`; a VOID shape clears the fills pushed since then.
pub(super) fn lower_user_shape(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    shape: &ipc2581::types::UserShape,
    transform: Affine2,
    primitive_start: usize,
) -> Result<()> {
    let path_start = doc.arena.paths.len() as u32;
    let mut void = false;
    let mut outline = None;
    let mut centerline = None;
    match &shape.shape {
        UserShapeType::Circle(circle) => outline = shapes::circle(circle.diameter),
        UserShapeType::RectCenter(rect) => {
            outline = shapes::rect(rect.size.width, rect.size.height)
        }
        UserShapeType::Oval(oval) => outline = shapes::obround(oval.size.width, oval.size.height),
        UserShapeType::RectRound(rect) => outline = rect_round_outline(rect),
        UserShapeType::Polygon(polygon) => outline = Some(polygon_contour(polygon)),
        UserShapeType::Contour(contour) => {
            push_outline_path(doc, &contour.polygon, &contour.cutouts, transform);
        }
        UserShapeType::Line(line) => {
            centerline = Some(vec![
                PathCmd::move_to(Point::new(line.start.x, line.start.y)),
                PathCmd::line_to(Point::new(line.end.x, line.end.y)),
            ]);
        }
        UserShapeType::Arc(arc) => {
            centerline = Some(vec![
                PathCmd::move_to(Point::new(arc.start.x, arc.start.y)),
                arc_step(arc.end, arc.center, arc.clockwise),
            ]);
        }
        UserShapeType::Polyline(polyline) => centerline = Some(poly_step_commands(polyline)),
        UserShapeType::StandardPrimitive(primitive) => {
            void = lower_standard_primitive(context, doc, primitive, transform)?;
        }
        UserShapeType::StandardPrimitiveRef(primitive_ref) => {
            if let Some(primitive) = context.standard_primitives.get(primitive_ref).copied() {
                void = lower_standard_primitive(context, doc, primitive, transform)?;
            } else {
                doc.warn(format!(
                    "Not drawing nested standard primitive '{}': it is missing",
                    context.strings.resolve(*primitive_ref)
                ));
            }
        }
        // Text has no glyphs here, and KiCad's zero-width glyph Outlines have
        // no area to image.
        UserShapeType::Text(_) | UserShapeType::Outline(_) => {}
        UserShapeType::UserPrimitive(primitive) => {
            lower_user_primitive(context, doc, primitive, transform)?;
        }
        UserShapeType::UserPrimitiveRef(primitive_ref) => {
            if let Some(primitive) = context.user_primitives.get(primitive_ref).copied() {
                lower_user_primitive(context, doc, primitive, transform)?;
            } else {
                doc.warn(format!(
                    "Not drawing nested user primitive '{}': it is missing",
                    context.strings.resolve(*primitive_ref)
                ));
            }
        }
    }
    push_filled_shape(doc, transform, outline);
    // An open contour stays unpainted until `paint_paths` strokes it.
    let strokes = centerline.is_some();
    if let Some(cmds) = centerline {
        let contour = ContourBuf::new(cmds).with_consistent_arcs();
        doc.push_path(Paint::None, [contour.transformed(transform)]);
    }

    let fill_desc = shape.fill_desc.as_deref().copied().or_else(|| {
        shape
            .fill_desc_ref
            .and_then(|id| context.fill_descs.get(&id).copied())
    });
    let hollow = fill_desc.is_some_and(|fill| fill.fill_property == FillProperty::Hollow);
    if strokes || hollow {
        let resolve = if strokes {
            require_line_desc
        } else {
            resolve_line_desc
        };
        let line_desc = resolve(
            context,
            doc,
            "user shape outline",
            shape.line_desc_ref,
            shape.line_desc,
        );
        paint_paths(doc, path_start, line_desc, transform);
    }
    let fill = fill_desc.map(|fill| fill.fill_property);
    // IPC-2581C §3.5.6.1: a VOID clears only the fills before it in its own
    // UserSpecial, never strokes, later islands or other primitives.
    if void || fill == Some(FillProperty::Void) {
        subtract_trailing_paths(
            doc,
            primitive_start,
            path_start as usize,
            context.resolution,
        )?;
    } else {
        warn_patterned_fill(doc, fill);
    }
    Ok(())
}

/// Subtract the fills pushed since `cutter_start` from the fills in
/// `subject_start..cutter_start`, consuming the cutters. Subjects the cutters
/// do not reach keep their exact source curves.
pub(super) fn subtract_trailing_paths(
    doc: &mut GeometryDocument,
    subject_start: usize,
    cutter_start: usize,
    resolution: Resolution,
) -> Result<()> {
    let cutters = ContourSet::from_painted_paths(
        &doc.arena,
        doc.arena.paths[cutter_start..]
            .iter()
            .filter(|path| path.is_filled()),
        resolution.strict(),
    )?;
    doc.arena.paths.truncate(cutter_start);
    for index in subject_start..cutter_start {
        let path = doc.arena.paths[index];
        let Some(rule) = path.fill_rule() else {
            continue;
        };
        if !path.bbox.intersects(cutters.bbox) {
            continue;
        }
        let subject =
            ContourSet::from_contours(&doc.arena.path_contours(&path), rule, resolution.strict())?;
        let result = subject.difference(&cutters)?;
        let (contours, bbox) = doc.arena.push_contours(result.to_contours());
        doc.arena.paths[index] = Path {
            contours,
            bbox,
            paint: if result.is_empty() {
                Paint::None
            } else {
                Paint::Fill {
                    rule: FillRule::NonZero,
                }
            },
        };
    }
    Ok(())
}

/// HATCH and MESH fills are painted solid, which overstates their copper.
pub(super) fn warn_patterned_fill(doc: &mut GeometryDocument, fill: Option<FillProperty>) {
    if matches!(fill, Some(FillProperty::Hatch | FillProperty::Mesh)) {
        doc.warn("Painting a HATCH or MESH fill solid because patterned fills are not imported");
    }
}

pub(super) fn primitive_fill_property(
    context: &ExtractContext<'_>,
    primitive: &StandardPrimitive,
) -> Option<FillProperty> {
    let style = primitive_style(primitive);
    style.fill_property.or_else(|| {
        style
            .fill_desc_ref
            .and_then(|reference| context.fill_descs.get(&reference))
            .map(|description| description.fill_property)
    })
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct StandardPrimitiveStyle {
    pub(super) fill_property: Option<FillProperty>,
    pub(super) line_desc: Option<ipc2581::types::LineDesc>,
    pub(super) line_desc_ref: Option<Symbol>,
    pub(super) fill_desc_ref: Option<Symbol>,
}

pub(super) fn primitive_style(primitive: &StandardPrimitive) -> StandardPrimitiveStyle {
    fn styled<T>(styled: &ipc2581::types::Styled<T>) -> StandardPrimitiveStyle {
        StandardPrimitiveStyle {
            fill_property: styled
                .fill_desc
                .map(|description| description.fill_property)
                .or(styled.fill_property),
            line_desc: styled.line_desc,
            line_desc_ref: styled.line_desc_ref,
            fill_desc_ref: styled.fill_desc_ref,
        }
    }

    match primitive {
        StandardPrimitive::Circle(value) => styled(value),
        StandardPrimitive::RectCenter(value) => styled(value),
        StandardPrimitive::RectRound(value) => styled(value),
        StandardPrimitive::RectCham(value) => styled(value),
        StandardPrimitive::RectCorner(value) => styled(value),
        StandardPrimitive::Oval(value) => styled(value),
        StandardPrimitive::Butterfly(value) => styled(value),
        StandardPrimitive::Diamond(value) => styled(value),
        StandardPrimitive::Donut(value) => styled(value),
        StandardPrimitive::Ellipse(value) => styled(value),
        StandardPrimitive::Hexagon(value) => styled(value),
        StandardPrimitive::Octagon(value) => styled(value),
        StandardPrimitive::Thermal(value) => styled(value),
        StandardPrimitive::Triangle(value) => styled(value),
        StandardPrimitive::Moire(_) | StandardPrimitive::Contour(_) => {
            StandardPrimitiveStyle::default()
        }
    }
}

/// Stroke the outlines pushed since `path_start` with `line_desc`, scaled
/// with the placement like the outlines themselves. Without a description
/// they stay unpainted: no width is invented.
pub(super) fn paint_paths(
    doc: &mut GeometryDocument,
    path_start: u32,
    line_desc: Option<ipc2581::types::LineDesc>,
    transform: Affine2,
) {
    let paint = line_desc.map_or(Paint::None, |line_desc| {
        stroke_paint(line_desc, placement_scale(transform))
    });
    let half_width = paint.stroke().map_or(0.0, |stroke| stroke.width / 2.0);
    for index in path_start as usize..doc.arena.paths.len() {
        let contours = doc.arena.paths[index].contours;
        let bbox = doc.arena.contours_bbox(contours).expand(half_width);
        let path = &mut doc.arena.paths[index];
        path.paint = paint;
        path.bbox = bbox;
    }
}

pub(super) fn poly_step_commands(polygon: &ipc2581::types::Polygon) -> Vec<PathCmd> {
    let begin = polygon.begin();
    std::iter::once(PathCmd::move_to(Point::new(begin.x, begin.y)))
        .chain(polygon.steps().map(|step| match step {
            PolyStep::Segment(segment) => {
                PathCmd::line_to(Point::new(segment.point.x, segment.point.y))
            }
            PolyStep::Curve(curve) => arc_step(curve.point, curve.center, curve.clockwise),
        }))
        .collect()
}

/// A circular step to `end` about `center`. A step onto its own center has
/// no radius to sweep — KiCad writes one where an arc collapses to a point —
/// so it is reached straight, and a stroke of it images its cap.
pub(super) fn arc_step(
    end: ipc2581::types::Point,
    center: ipc2581::types::Point,
    clockwise: bool,
) -> PathCmd {
    let (end, center) = (Point::new(end.x, end.y), Point::new(center.x, center.y));
    if end == center {
        PathCmd::line_to(end)
    } else {
        PathCmd::arc_to(end, center, clockwise)
    }
}

pub(super) fn polygon_contour(polygon: &ipc2581::types::Polygon) -> ContourBuf {
    let mut cmds = poly_step_commands(polygon);
    cmds.push(PathCmd::close());
    ContourBuf::new(cmds).with_consistent_arcs()
}

/// An outline with its cutouts as one even-odd path; returns the path index.
///
/// Even-odd imaging equals outline minus cutouts only while every cutout
/// stays inside the outline and clear of its siblings. Overlapping siblings
/// take a boolean to find; a cutout leaving the outline's bounds is certain
/// and paints outside it, so that much is reported.
pub(super) fn push_outline_path(
    doc: &mut GeometryDocument,
    outline: &ipc2581::types::Polygon,
    cutouts: &[ipc2581::types::Polygon],
    transform: Affine2,
) -> u32 {
    let contours = std::iter::once(outline)
        .chain(cutouts)
        .map(|polygon| polygon_contour(polygon).transformed(transform))
        .collect::<Vec<_>>();
    let bounds = contours[0].bbox.expand(tol::REGION_MM);
    if contours[1..].iter().any(|cutout| {
        !bounds.contains_point(cutout.bbox.min) || !bounds.contains_point(cutout.bbox.max)
    }) {
        doc.warn("A Contour cutout reaches outside its outline and paints there");
    }
    doc.push_path(
        Paint::Fill {
            rule: FillRule::EvenOdd,
        },
        contours,
    )
}

// Outlines of the shapes standard and user primitives share.

pub(super) fn rect_round_outline(rect: &ipc2581::types::RectRound) -> Option<ContourBuf> {
    shapes::rounded_rect(
        rect.size.width,
        rect.size.height,
        rect.radius,
        [
            rect.upper_right,
            rect.lower_right,
            rect.lower_left,
            rect.upper_left,
        ],
    )
}

pub(super) fn push_filled_shape(
    doc: &mut GeometryDocument,
    transform: Affine2,
    contour: Option<ContourBuf>,
) {
    if let Some(contour) = contour {
        doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [contour.transformed(transform)],
        );
    }
}

/// The outline both Donut and Thermal rings are built from. Polygonal shapes
/// follow their standalone primitives: a square by its side, a hexagon or
/// octagon by its point-to-point diameter with a vertex pointing down.
pub(super) fn concentric_outline(shape: ConcentricShape, diameter: f64) -> Option<ContourBuf> {
    match shape {
        ConcentricShape::Round => shapes::circle(diameter),
        ConcentricShape::Square => shapes::rect(diameter, diameter),
        ConcentricShape::Hexagon => shapes::regular_polygon(diameter, 6, -90.0),
        ConcentricShape::Octagon => shapes::regular_polygon(diameter, 8, -90.0),
    }
}

pub(super) fn push_ring_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    shape: ConcentricShape,
    outer_diameter: f64,
    inner_diameter: f64,
) {
    doc.push_path(
        Paint::Fill {
            rule: FillRule::EvenOdd,
        },
        [outer_diameter, inner_diameter]
            .into_iter()
            .filter_map(|diameter| concentric_outline(shape, diameter))
            .map(|outline| outline.transformed(transform)),
    );
}

pub(super) fn push_butterfly_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    shape: ipc2581::types::ButterflyShape,
    size: f64,
) {
    let radius = size / 2.0;
    let quadrant = |x: f64, y: f64| {
        let center = Point::new(x * radius / 2.0, y * radius / 2.0);
        shapes::rect(radius, radius)
            .unwrap_or_default()
            .transformed(transform.concat(Affine2::translation(center)))
    };
    match shape {
        ipc2581::types::ButterflyShape::Round => doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [
                circular_sector_contour(transform, radius, 90.0, 180.0),
                circular_sector_contour(transform, radius, 270.0, 360.0),
            ],
        ),
        ipc2581::types::ButterflyShape::Square => doc.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [quadrant(-1.0, 1.0), quadrant(1.0, -1.0)],
        ),
    };
}

pub(super) fn push_moire_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    moire: &ipc2581::types::Moire,
) {
    for index in 0..moire.ring_number {
        let centerline_diameter = moire.diameter - 2.0 * index as f64 * moire.ring_gap;
        let outer_diameter = centerline_diameter + moire.ring_width;
        let inner_diameter = centerline_diameter - moire.ring_width;
        if outer_diameter <= 0.0 {
            break;
        }

        if inner_diameter > 0.0 {
            push_ring_path(
                doc,
                transform,
                ConcentricShape::Round,
                outer_diameter,
                inner_diameter,
            );
        } else {
            push_filled_shape(
                doc,
                transform,
                shapes::ellipse(outer_diameter, outer_diameter),
            );
        }
    }

    if let (Some(width), Some(length)) = (moire.line_width, moire.line_length) {
        let angle = moire.line_angle.unwrap_or(0.0);
        for line_angle in [angle, angle + 90.0] {
            push_filled_shape(
                doc,
                transform.concat(Affine2::placement(
                    Point::default(),
                    line_angle,
                    Mirror::NONE,
                    1.0,
                )),
                shapes::rect(length, width),
            );
        }
    }
}

/// IPC-2581C 3.5.9.15: a thermal is its ring with `spokeCount` spokes cut out
/// of it, each `spokeWidth` between its sides (by default the ring's diameter
/// difference) and the first at `spokeStartAngle` (by default 45 degrees).
/// Without spokes it is the donut.
pub(super) fn push_thermal_path(
    doc: &mut GeometryDocument,
    transform: Affine2,
    thermal: &ipc2581::types::Thermal,
    resolution: Resolution,
) -> Result<()> {
    let ring_start = doc.arena.paths.len();
    push_ring_path(
        doc,
        transform,
        thermal.shape,
        thermal.outer_diameter,
        thermal.inner_diameter,
    );

    let cut_start = doc.arena.paths.len();
    let spoke_width = thermal
        .spoke_width
        .unwrap_or(thermal.outer_diameter - thermal.inner_diameter);
    let spoke_start_angle = thermal.spoke_start_angle.unwrap_or(45.0);
    // Reaches past the corners of every outline shape.
    let length = thermal.outer_diameter;
    for index in 0..thermal.spoke_count {
        let angle = spoke_start_angle + index as f64 * 360.0 / thermal.spoke_count as f64;
        let (sin, cos) = angle.to_radians().sin_cos();
        let cut = Affine2::placement(
            Point::new(length / 2.0 * cos, length / 2.0 * sin),
            angle,
            Mirror::NONE,
            1.0,
        );
        push_filled_shape(
            doc,
            transform.concat(cut),
            shapes::rect(length, spoke_width),
        );
    }
    subtract_trailing_paths(doc, ring_start, cut_start, resolution)
}

pub(super) fn circular_sector_contour(
    transform: Affine2,
    radius: f64,
    start_degrees: f64,
    end_degrees: f64,
) -> ContourBuf {
    let start_angle = start_degrees.to_radians();
    let end_angle = end_degrees.to_radians();
    let start = Point::new(radius * start_angle.cos(), radius * start_angle.sin());
    let end = Point::new(radius * end_angle.cos(), radius * end_angle.sin());
    ContourBuf::new(vec![
        PathCmd::move_to(Point::default()),
        PathCmd::line_to(start),
        PathCmd::arc_to(end, Point::default(), false),
        PathCmd::close(),
    ])
    .transformed(transform)
}

pub(super) fn map_polarity(polarity: Polarity) -> GeometryPolarity {
    match polarity {
        Polarity::Positive => GeometryPolarity::Dark,
        Polarity::Negative => GeometryPolarity::Clear,
    }
}

pub(super) fn map_line_cap(line_end: LineEnd) -> LineCap {
    match line_end {
        LineEnd::None => LineCap::Butt,
        LineEnd::Round => LineCap::Round,
        LineEnd::Square => LineCap::Square,
    }
}

pub(super) fn map_line_pattern(line_property: Option<LineProperty>) -> LinePattern {
    match line_property {
        Some(LineProperty::Solid) | None => LinePattern::Solid,
        Some(LineProperty::Dotted) => LinePattern::Dotted,
        Some(LineProperty::Dashed) => LinePattern::Dashed,
        Some(LineProperty::Center) => LinePattern::Center,
        Some(LineProperty::Phantom) => LinePattern::Phantom,
        Some(LineProperty::Erase) => LinePattern::Erase,
    }
}
