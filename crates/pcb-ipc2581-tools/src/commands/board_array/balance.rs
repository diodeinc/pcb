//! Board-array adapter for the shared dense copper balancing engine.
//!
//! Safe-region discovery is deliberately outside this module. Once a caller
//! supplies that region and the measured layer areas, this converts the
//! locally resolved positive copper image into IPC-2581 polygon features.

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    ecad::SetFeature,
    primitives::{Point as IpcPoint, PolyStep, PolyStepCurve, PolyStepSegment, Polygon},
};
use pcb_ir::geom::copper_balance::{
    DenseCopperBalanceProfile, DenseCopperBalanceRequest, DenseCopperBalanceResult,
    generate_dense_copper_balance,
};
use pcb_ir::geom::{ContourBuf, ContourSet, PathOp};

/// Solve and convert one layer's explicit safe region to positive IPC polygons.
pub fn generate_board_array_copper_balance(
    profile: DenseCopperBalanceProfile,
    request: DenseCopperBalanceRequest<'_>,
) -> Result<(DenseCopperBalanceResult, Vec<SetFeature>)> {
    let result = generate_dense_copper_balance(profile, request)?;
    let features = ipc_polygon_features(&result.copper)?;
    Ok((result, features))
}

fn ipc_polygon_features(region: &ContourSet) -> Result<Vec<SetFeature>> {
    region
        .to_bridged_contours_with_arcs()
        .iter()
        .map(ipc_polygon_from_contour)
        .map(|polygon| polygon.map(SetFeature::Polygon))
        .collect()
}

fn ipc_polygon_from_contour(contour: &ContourBuf) -> Result<Polygon> {
    let mut begin = None;
    let mut steps = Vec::new();

    for command in &contour.cmds {
        match command.op {
            PathOp::MoveTo if begin.is_none() => {
                begin = Some(IpcPoint {
                    x: command.p0.x,
                    y: command.p0.y,
                });
            }
            PathOp::LineTo if begin.is_some() => {
                steps.push(PolyStep::Segment(PolyStepSegment {
                    point: IpcPoint {
                        x: command.p0.x,
                        y: command.p0.y,
                    },
                }));
            }
            PathOp::ArcTo if begin.is_some() => {
                steps.push(PolyStep::Curve(PolyStepCurve {
                    point: IpcPoint {
                        x: command.p0.x,
                        y: command.p0.y,
                    },
                    center: IpcPoint {
                        x: command.p1.x,
                        y: command.p1.y,
                    },
                    clockwise: command.clockwise,
                }));
            }
            PathOp::Close if begin.is_some() => {}
            PathOp::CubicTo => {
                bail!("generated copper balance polygon contains an unsupported cubic segment")
            }
            _ => bail!("generated copper balance polygon contains multiple or missing contours"),
        }
    }

    let begin = begin.context("generated copper balance polygon has no starting point")?;
    if steps.len() < 2 {
        bail!("generated copper balance polygon has fewer than three vertices");
    }
    Ok(Polygon { begin, steps })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::geom::{BBox, ContourSet, Point, tol};

    #[test]
    fn converts_perforated_region_to_positive_ipc_polygons() {
        let safe_region = ContourSet::rectangle(
            BBox::new(Point::new(0.0, 0.0), Point::new(20.0, 10.0)),
            tol::REGION_MM,
        );
        let (result, features) = generate_board_array_copper_balance(
            DenseCopperBalanceProfile::V1,
            DenseCopperBalanceRequest {
                safe_region: &safe_region,
                retained_area_mm2: 200.0,
                existing_copper_area_mm2: 0.0,
                target_density: 0.75,
                lattice_origin: Point::new(10.0, 5.0),
            },
        )
        .unwrap();

        assert!(!result.copper.is_empty());
        assert!(
            features
                .iter()
                .all(|feature| matches!(feature, SetFeature::Polygon(_)))
        );
        assert!(
            features
                .iter()
                .filter_map(|feature| match feature {
                    SetFeature::Polygon(polygon) => Some(polygon),
                    _ => None,
                })
                .flat_map(|polygon| &polygon.steps)
                .any(|step| matches!(step, PolyStep::Curve(_)))
        );
    }
}
