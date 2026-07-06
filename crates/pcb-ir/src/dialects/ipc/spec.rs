use crate::geom::Span;

/// A named IPC-2581 `Spec` definition.
#[derive(Debug, Clone)]
pub struct Spec<Symbol> {
    pub name: Symbol,
    /// Spans `doc.spec_items`.
    pub items: Span,
}

#[derive(Debug, Clone)]
pub struct SpecItem<Symbol> {
    pub element: Symbol,
    pub kind: SpecItemKind,
    pub item_type: Option<Symbol>,
    pub comment: Option<Symbol>,
    /// Spans `doc.spec_properties`.
    pub properties: Span,
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

#[derive(Debug, Clone)]
pub struct SpecProperty<Symbol> {
    pub value: Option<f64>,
    pub text: Option<Symbol>,
    pub unit: Option<Symbol>,
    pub plus_tol: Option<f64>,
    pub minus_tol: Option<f64>,
    pub tol_percent: Option<bool>,
}

/// A reference from a layer or feature set to a named spec.
#[derive(Debug, Clone)]
pub struct SpecRef<Symbol> {
    pub spec: Symbol,
}
