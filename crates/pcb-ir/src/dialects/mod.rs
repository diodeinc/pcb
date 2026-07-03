pub mod artwork;
pub mod ipc;
pub mod mask;
pub mod nc;
pub mod placement;

/// Fabrication role of a physical layer image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LayerRole {
    Copper,
    Soldermask,
    Paste,
    Legend,
    Profile,
    Drill,
    Mechanical,
    Other,
}

/// Which side of the board a layer or feature belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Top,
    Bottom,
    Inner,
    None,
}
