use pcb_ir::dialects::artwork::legalize::TargetCapabilities;

/// Gerber subset selected for the downstream CAM importer.
///
/// Both dialects are valid Gerber. `Jlcpcb` deliberately avoids optional
/// constructs that JLCPCB's order-preview importer is known to misinterpret.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GerberDialect {
    /// Full Gerber output following the Ucamco specification.
    Standard,
    /// Standards-compliant subset accepted reliably by JLCPCB.
    #[default]
    Jlcpcb,
}

/// A confirmed downstream Gerber importer defect and its legalization rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GerberImporterBug {
    pub id: &'static str,
    pub symptom: &'static str,
    pub workaround: &'static str,
}

/// Known JLCPCB Gerber importer bugs that define [`GerberDialect::Jlcpcb`].
///
/// - `JLC-001`: Parameterized aperture macros using the valid center-line
///   primitive code 21 are rendered with oversized geometry. This was
///   reproduced on Button v0.1.3 solder-mask rounded rectangles. Emit an
///   equivalent code-4 contour aperture instead.
/// - `JLC-002`: `%LR` load rotation on a valid off-origin custom aperture is
///   applied about the wrong point, shifting the flashed geometry. This was
///   reproduced on the Button v0.1.3 SG07320N bottom-copper pad. Bake the
///   aperture's complete linear transform into its shared definition and
///   leave only translation on the flash.
pub const JLCPCB_IMPORTER_BUGS: &[GerberImporterBug] = &[
    GerberImporterBug {
        id: "JLC-001",
        symptom: "primitive-21 aperture macros render oversized",
        workaround: "replace rounded rectangles with code-4 contour apertures",
    },
    GerberImporterBug {
        id: "JLC-002",
        symptom: "load rotation shifts off-origin custom apertures",
        workaround: "bake aperture transforms into shared aperture definitions",
    },
];

impl GerberDialect {
    pub(crate) fn artwork_capabilities(self) -> TargetCapabilities {
        match self {
            Self::Standard => TargetCapabilities::NATIVE,
            Self::Jlcpcb => TargetCapabilities {
                // JLC-001 in `JLCPCB_IMPORTER_BUGS`.
                round_rect_apertures: false,
                // JLC-002 in `JLCPCB_IMPORTER_BUGS`. Bake the complete basis
                // so no LM/LR/LS load-state ordering can reintroduce it.
                aperture_transforms: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jlc_dialect_references_every_known_importer_bug() {
        assert_eq!(
            JLCPCB_IMPORTER_BUGS
                .iter()
                .map(|bug| bug.id)
                .collect::<Vec<_>>(),
            ["JLC-001", "JLC-002"]
        );
        assert_eq!(
            GerberDialect::Jlcpcb.artwork_capabilities(),
            TargetCapabilities {
                round_rect_apertures: false,
                aperture_transforms: false,
            }
        );
    }
}
