//! Semantic surface-layer resolution for physical board features.

use std::fmt;

use crate::dialects::{LayerRole, Side};

/// One declared physical layer, classified independently of its source name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalLayer<Symbol> {
    pub name: Symbol,
    pub role: LayerRole,
    pub side: Side,
}

impl<Symbol> PhysicalLayer<Symbol> {
    pub fn new(name: Symbol, role: LayerRole, side: Side) -> Self {
        Self { name, role, side }
    }
}

/// The four declared artwork layers needed for two-sided fiducials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TwoSidedSurfaceLayers<Symbol> {
    pub top_copper: Symbol,
    pub top_soldermask: Symbol,
    pub bottom_copper: Symbol,
    pub bottom_soldermask: Symbol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceLayerError {
    Missing {
        role: LayerRole,
        side: Side,
    },
    Ambiguous {
        role: LayerRole,
        side: Side,
        count: usize,
    },
}

impl fmt::Display for SurfaceLayerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { role, side } => write!(
                f,
                "missing {} {} layer required for two-sided surface features",
                side_name(*side),
                role_name(*role),
            ),
            Self::Ambiguous { role, side, count } => write!(
                f,
                "found {count} {} {} layers; expected exactly one for two-sided surface features",
                side_name(*side),
                role_name(*role),
            ),
        }
    }
}

impl std::error::Error for SurfaceLayerError {}

/// Resolve the declared outer copper and solder-mask layers by semantics.
///
/// Physical layers are never inferred or created. Every required role and side
/// must have exactly one declaration, regardless of its source layer name.
pub fn resolve_two_sided_surface_layers<Symbol: Copy>(
    layers: impl IntoIterator<Item = PhysicalLayer<Symbol>>,
) -> Result<TwoSidedSurfaceLayers<Symbol>, SurfaceLayerError> {
    let layers = layers.into_iter().collect::<Vec<_>>();
    Ok(TwoSidedSurfaceLayers {
        top_copper: resolve_one(&layers, LayerRole::Copper, Side::Top)?,
        top_soldermask: resolve_one(&layers, LayerRole::Soldermask, Side::Top)?,
        bottom_copper: resolve_one(&layers, LayerRole::Copper, Side::Bottom)?,
        bottom_soldermask: resolve_one(&layers, LayerRole::Soldermask, Side::Bottom)?,
    })
}

fn resolve_one<Symbol: Copy>(
    layers: &[PhysicalLayer<Symbol>],
    role: LayerRole,
    side: Side,
) -> Result<Symbol, SurfaceLayerError> {
    let mut matches = layers
        .iter()
        .filter(|layer| layer.role == role && layer.side == side);
    let Some(layer) = matches.next() else {
        return Err(SurfaceLayerError::Missing { role, side });
    };
    let count = 1 + matches.count();
    if count > 1 {
        return Err(SurfaceLayerError::Ambiguous { role, side, count });
    }
    Ok(layer.name)
}

fn role_name(role: LayerRole) -> &'static str {
    match role {
        LayerRole::Copper => "copper",
        LayerRole::Soldermask => "solder-mask",
        LayerRole::Paste => "paste",
        LayerRole::Legend => "legend",
        LayerRole::Profile => "profile",
        LayerRole::Drill => "drill",
        LayerRole::Mechanical => "mechanical",
        LayerRole::Other => "other",
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Top => "top",
        Side::Bottom => "bottom",
        Side::Inner => "inner",
        Side::None => "side-neutral",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_two_sided_layers_by_role_and_side_not_name() {
        let resolved = resolve_two_sided_surface_layers([
            PhysicalLayer::new(10, LayerRole::Soldermask, Side::Bottom),
            PhysicalLayer::new(20, LayerRole::Copper, Side::Top),
            PhysicalLayer::new(30, LayerRole::Other, Side::Top),
            PhysicalLayer::new(40, LayerRole::Copper, Side::Bottom),
            PhysicalLayer::new(50, LayerRole::Soldermask, Side::Top),
            PhysicalLayer::new(60, LayerRole::Copper, Side::Inner),
        ])
        .unwrap();

        assert_eq!(
            resolved,
            TwoSidedSurfaceLayers {
                top_copper: 20,
                top_soldermask: 50,
                bottom_copper: 40,
                bottom_soldermask: 10,
            }
        );
    }

    #[test]
    fn rejects_a_missing_surface_layer() {
        let error = resolve_two_sided_surface_layers([
            PhysicalLayer::new(10, LayerRole::Copper, Side::Top),
            PhysicalLayer::new(20, LayerRole::Soldermask, Side::Top),
            PhysicalLayer::new(30, LayerRole::Copper, Side::Bottom),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            SurfaceLayerError::Missing {
                role: LayerRole::Soldermask,
                side: Side::Bottom,
            }
        );
    }

    #[test]
    fn rejects_an_ambiguous_surface_layer() {
        let error = resolve_two_sided_surface_layers([
            PhysicalLayer::new(10, LayerRole::Copper, Side::Top),
            PhysicalLayer::new(11, LayerRole::Copper, Side::Top),
            PhysicalLayer::new(20, LayerRole::Soldermask, Side::Top),
            PhysicalLayer::new(30, LayerRole::Copper, Side::Bottom),
            PhysicalLayer::new(40, LayerRole::Soldermask, Side::Bottom),
        ])
        .unwrap_err();

        assert_eq!(
            error,
            SurfaceLayerError::Ambiguous {
                role: LayerRole::Copper,
                side: Side::Top,
                count: 2,
            }
        );
    }
}
