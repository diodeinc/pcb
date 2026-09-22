use crate::geom::Span;
use ipc2581::Symbol;

/// A named IPC-2581 `Spec` definition.
#[derive(Debug, Clone)]
pub struct Spec {
    pub name: Symbol,
    /// Spans `doc.spec_items`.
    pub items: Span,
}

#[derive(Debug, Clone)]
pub struct SpecItem {
    pub kind: SpecItemKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecItemKind {
    General,
    Dielectric,
    Conductor,
    SurfaceFinish,
    VCut,
    Other,
}

/// A reference from a layer or feature set to a named spec.
#[derive(Debug, Clone)]
pub struct SpecRef {
    pub spec: Symbol,
}
