use pcb_ir::dialects::artwork::legalize::TargetCapabilities;

/// Gerber output dialect.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GerberDialect {
    /// Full Gerber output following the Ucamco specification.
    Standard,
    /// JLCPCB-compatible subset.
    ///
    /// Avoids confirmed importer bugs:
    ///
    /// - primitive 21 rounded rectangles render oversized;
    /// - `%LR` shifts off-origin custom apertures.
    #[default]
    Jlcpcb,
}

impl GerberDialect {
    pub(crate) fn artwork_capabilities(self) -> TargetCapabilities {
        match self {
            Self::Standard => TargetCapabilities::NATIVE,
            Self::Jlcpcb => TargetCapabilities {
                round_rect_apertures: false,
                aperture_transforms: false,
            },
        }
    }
}
