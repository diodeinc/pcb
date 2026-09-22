//! Aperture rewrites for targets that cannot transform flashes.

use super::{Aperture, ApertureShape};
use crate::geom::path::ContourBuf;
use crate::geom::{Affine2, Point};

const TRANSFORM_EPSILON: f64 = 1e-9;

/// Apply a translation-free affine basis directly to an aperture definition.
pub fn bake_aperture_basis(aperture: &Aperture, basis: Affine2) -> Aperture {
    let similarity = basis.preserves_circles(TRANSFORM_EPSILON);
    let scale = basis.m00.hypot(basis.m10);
    let sized = |width: f64, height: f64| axis_aligned_dimensions(width, height, basis);
    let shape =
        match &aperture.shape {
            ApertureShape::Circle { diameter } if similarity => Some(ApertureShape::Circle {
                diameter: diameter * scale,
            }),
            ApertureShape::Rectangle { width, height } => sized(*width, *height)
                .map(|(width, height)| ApertureShape::Rectangle { width, height }),
            ApertureShape::Obround { width, height } => sized(*width, *height)
                .map(|(width, height)| ApertureShape::Obround { width, height }),
            ApertureShape::Polygon {
                diameter,
                vertices,
                rotation_degrees,
            } if similarity => {
                let radians = rotation_degrees.to_radians();
                let first_vertex = basis.transform_vector(Point::new(radians.cos(), radians.sin()));
                Some(ApertureShape::Polygon {
                    diameter: diameter * scale,
                    vertices: *vertices,
                    rotation_degrees: first_vertex.y.atan2(first_vertex.x).to_degrees(),
                })
            }
            ApertureShape::RoundRect {
                width,
                height,
                radius,
            } => sized(*width, *height).map(|(width, height)| ApertureShape::RoundRect {
                width,
                height,
                radius: radius * scale,
            }),
            ApertureShape::Contour { .. }
            | ApertureShape::Circle { .. }
            | ApertureShape::Polygon { .. } => None,
        };
    match shape {
        Some(shape) => Aperture {
            shape,
            hole_diameter: aperture.hole_diameter * scale,
        },
        None => contour_aperture(aperture, basis),
    }
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

fn contour_aperture(aperture: &Aperture, basis: Affine2) -> Aperture {
    let fill_rule = aperture.fill_rule();
    let contours = aperture
        .contours()
        .into_iter()
        .map(|contour| contour.transformed(basis))
        .collect::<Vec<_>>();
    let uncertainty_mm = contours
        .iter()
        .map(|contour| contour.uncertainty_mm)
        .fold(0.0, f64::max);
    let cmds = contours
        .into_iter()
        .flat_map(|contour| contour.cmds)
        .collect();
    Aperture::solid(ApertureShape::Contour {
        outline: ContourBuf::new(cmds).with_uncertainty(uncertainty_mm),
        fill_rule,
    })
}
