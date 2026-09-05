//! Gerber importer legalization for ordered artwork.

use super::{Aperture, ApertureShape, Document, Geometry, normalize_bounds};
use crate::geom::path::ContourBuf;
use crate::geom::{AccuracyError, GeometryAccuracy};
use crate::geom::{Affine2, Point};

const TRANSFORM_EPSILON: f64 = 1e-9;

/// Rewrite artwork for JLCPCB's Gerber importer while retaining shared flashes.
///
/// JLCPCB renders primitive 21 rounded rectangles oversized and shifts
/// off-origin custom apertures using `%LR`.
pub fn legalize_for_jlcpcb<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    replace_round_rect_apertures(doc, accuracy)?;
    bake_flash_transforms(doc, accuracy)?;
    normalize_bounds(doc);

    Ok(())
}

fn replace_round_rect_apertures<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    for aperture in &mut doc.apertures {
        if matches!(aperture.shape, ApertureShape::RoundRect { .. }) {
            *aperture = contour_aperture(aperture, Affine2::IDENTITY, accuracy)?;
        }
    }
    Ok(())
}

fn bake_flash_transforms<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    accuracy: GeometryAccuracy,
) -> Result<(), AccuracyError> {
    for object_index in 0..doc.objects.len() {
        let geometry = doc.objects[object_index].geometry;
        doc.objects[object_index].geometry = legalize_flash_geometry(doc, geometry, accuracy)?;
    }
    for block_index in 0..doc.blocks.len() {
        for object_index in 0..doc.blocks[block_index].objects.len() {
            let geometry = doc.blocks[block_index].objects[object_index].geometry;
            let geometry = legalize_flash_geometry(doc, geometry, accuracy)?;
            doc.blocks[block_index].objects[object_index].geometry = geometry;
        }
    }
    Ok(())
}

fn legalize_flash_geometry<LayerMeta, ObjectMeta>(
    doc: &mut Document<LayerMeta, ObjectMeta>,
    geometry: Geometry,
    accuracy: GeometryAccuracy,
) -> Result<Geometry, AccuracyError> {
    let Geometry::Flash {
        aperture,
        transform,
    } = geometry
    else {
        return Ok(geometry);
    };
    if transform.is_translation() {
        return Ok(geometry);
    }
    let Some(source) = doc.apertures.get(aperture as usize).cloned() else {
        doc.warn(format!(
            "Skipping target legalization for flash with missing aperture {aperture}"
        ));
        return Ok(geometry);
    };

    let basis = Affine2 {
        m02: 0.0,
        m12: 0.0,
        ..transform
    };
    let aperture = doc.push_aperture(bake_aperture_basis(&source, basis, accuracy)?);
    Ok(Geometry::Flash {
        aperture,
        transform: Affine2::translation(Point::new(transform.m02, transform.m12)),
    })
}

/// Apply a translation-free affine basis directly to an aperture definition.
pub fn bake_aperture_basis(
    aperture: &Aperture,
    basis: Affine2,
    accuracy: GeometryAccuracy,
) -> Result<Aperture, AccuracyError> {
    let similarity = basis.preserves_circles(TRANSFORM_EPSILON);
    let scale = basis.m00.hypot(basis.m10);
    let scaled_hole = || aperture.hole_diameter * scale;

    Ok(match &aperture.shape {
        ApertureShape::Circle { diameter } if similarity => Aperture {
            shape: ApertureShape::Circle {
                diameter: diameter * scale,
            },
            hole_diameter: scaled_hole(),
        },
        ApertureShape::Rectangle { width, height } => {
            if let Some((width, height)) = axis_aligned_dimensions(*width, *height, basis) {
                Aperture {
                    shape: ApertureShape::Rectangle { width, height },
                    hole_diameter: scaled_hole(),
                }
            } else {
                contour_aperture(aperture, basis, accuracy)?
            }
        }
        ApertureShape::Obround { width, height } => {
            if let Some((width, height)) = axis_aligned_dimensions(*width, *height, basis) {
                Aperture {
                    shape: ApertureShape::Obround { width, height },
                    hole_diameter: scaled_hole(),
                }
            } else {
                contour_aperture(aperture, basis, accuracy)?
            }
        }
        ApertureShape::Polygon {
            diameter,
            vertices,
            rotation_degrees,
        } if similarity => {
            let radians = rotation_degrees.to_radians();
            let first_vertex = basis.transform_vector(Point::new(radians.cos(), radians.sin()));
            Aperture {
                shape: ApertureShape::Polygon {
                    diameter: diameter * scale,
                    vertices: *vertices,
                    rotation_degrees: first_vertex.y.atan2(first_vertex.x).to_degrees(),
                },
                hole_diameter: scaled_hole(),
            }
        }
        ApertureShape::RoundRect {
            width,
            height,
            radius,
        } => {
            if let Some((width, height)) = axis_aligned_dimensions(*width, *height, basis) {
                Aperture {
                    shape: ApertureShape::RoundRect {
                        width,
                        height,
                        radius: radius * scale,
                    },
                    hole_diameter: scaled_hole(),
                }
            } else {
                contour_aperture(aperture, basis, accuracy)?
            }
        }
        ApertureShape::RoundedHex {
            radius,
            corner_radius,
            rotation_degrees,
        } if similarity => {
            let radians = rotation_degrees.to_radians();
            let first_vertex = basis.transform_vector(Point::new(radians.cos(), radians.sin()));
            Aperture {
                shape: ApertureShape::RoundedHex {
                    radius: radius * scale,
                    corner_radius: corner_radius * scale,
                    rotation_degrees: first_vertex.y.atan2(first_vertex.x).to_degrees(),
                },
                hole_diameter: scaled_hole(),
            }
        }
        ApertureShape::Contour { .. }
        | ApertureShape::Circle { .. }
        | ApertureShape::Polygon { .. }
        | ApertureShape::RoundedHex { .. } => contour_aperture(aperture, basis, accuracy)?,
    })
}

fn axis_aligned_dimensions(width: f64, height: f64, basis: Affine2) -> Option<(f64, f64)> {
    if !basis.preserves_circles(TRANSFORM_EPSILON) {
        return None;
    }
    let scale = basis.m00.hypot(basis.m10);
    let epsilon = TRANSFORM_EPSILON * scale.max(1.0);
    if basis.m01.abs() <= epsilon && basis.m10.abs() <= epsilon {
        Some((width * scale, height * scale))
    } else if basis.m00.abs() <= epsilon && basis.m11.abs() <= epsilon {
        Some((height * scale, width * scale))
    } else {
        None
    }
}

fn contour_aperture(
    aperture: &Aperture,
    basis: Affine2,
    accuracy: GeometryAccuracy,
) -> Result<Aperture, AccuracyError> {
    let fill_rule = aperture.fill_rule();
    let contours = aperture
        .contours()
        .into_iter()
        .map(|contour| contour.transformed(basis, accuracy))
        .collect::<Result<Vec<_>, _>>()?;
    let uncertainty_mm = contours
        .iter()
        .map(|contour| contour.uncertainty_mm)
        .fold(0.0, f64::max);
    let cmds = contours
        .into_iter()
        .flat_map(|contour| contour.cmds)
        .collect();
    Ok(Aperture::solid(ApertureShape::Contour {
        outline: ContourBuf::new(cmds).with_uncertainty(uncertainty_mm),
        fill_rule,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialects::artwork::compare::{CompareTolerance, compare_documents};
    use crate::dialects::artwork::{Layer, Object};
    use crate::dialects::{LayerRole, Side};
    use crate::geom::{BBox, Mirror, Polarity, Span};

    fn round_rect_document() -> Document<Vec<String>, ()> {
        let mut doc = Document::new();
        let aperture = doc.push_aperture(Aperture::solid(ApertureShape::RoundRect {
            width: 2.0,
            height: 1.0,
            radius: 0.2,
        }));
        let layer = doc.push_layer(Layer {
            name: "F.Cu".to_string(),
            role: LayerRole::Copper,
            side: Side::Top,
            objects: Span::EMPTY,
            bbox: BBox::empty(),
            meta: vec!["Copper".to_string(), "L1".to_string(), "Top".to_string()],
        });
        for center in [Point::new(10.0, 20.0), Point::new(20.0, 20.0)] {
            doc.push_object(
                layer,
                Object::new(
                    Polarity::Dark,
                    Geometry::Flash {
                        aperture,
                        transform: Affine2::placement(center, 45.0, Mirror::NONE, 1.0),
                    },
                ),
            );
        }
        normalize_bounds(&mut doc);
        doc
    }

    #[test]
    fn legalizes_shapes_and_transforms_without_flattening_flashes() {
        let accuracy = GeometryAccuracy::default();

        let reference = round_rect_document();
        let mut candidate = reference.clone();
        legalize_for_jlcpcb(&mut candidate, accuracy).unwrap();

        assert!(
            candidate
                .apertures
                .iter()
                .all(|aperture| !matches!(aperture.shape, ApertureShape::RoundRect { .. }))
        );
        assert!(candidate.objects.iter().all(|object| matches!(
            object.geometry,
            Geometry::Flash { transform, .. } if transform.is_translation()
        )));
        let first = candidate.objects[0].geometry;
        let second = candidate.objects[1].geometry;
        assert!(matches!(
            (first, second),
            (
                Geometry::Flash { aperture: first, .. },
                Geometry::Flash { aperture: second, .. }
            ) if first == second
        ));

        let report = compare_documents(
            &reference,
            &candidate,
            CompareTolerance {
                bbox_mm: 1e-6,
                area_mm2: 1e-6,
            },
            accuracy,
        )
        .unwrap();
        assert!(report.is_match(), "{:#?}", report.mismatches);
    }
}
