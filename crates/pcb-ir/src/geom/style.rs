#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineCap {
    Round,
    Square,
    Butt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineJoin {
    Round,
    Miter,
    Bevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinePattern {
    Solid,
    Dotted,
    Dashed,
    Center,
    Phantom,
    Erase,
}

/// Paint polarity for layer imaging: dark adds material to the image, clear
/// removes it. IPC-2581 positive/negative polarity maps onto the same lattice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Polarity {
    Dark,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrokeStyle {
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub pattern: LinePattern,
}

impl StrokeStyle {
    pub fn new(width: f64, cap: LineCap) -> Self {
        Self {
            width,
            cap,
            join: LineJoin::Round,
            pattern: LinePattern::Solid,
        }
    }

    pub fn round(width: f64) -> Self {
        Self::new(width, LineCap::Round)
    }
}

/// How a path's contours are painted.
///
/// `Fill` treats contours as region boundaries under the given fill rule.
/// `Stroke` sweeps the styled pen along the contours. `None` marks physical
/// outline geometry (e.g. step profiles) that is not painted at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Paint {
    None,
    Fill { rule: FillRule },
    Stroke(StrokeStyle),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PaintKind {
    None,
    Fill,
    Stroke,
}

impl Paint {
    pub fn kind(&self) -> PaintKind {
        match self {
            Self::None => PaintKind::None,
            Self::Fill { .. } => PaintKind::Fill,
            Self::Stroke(_) => PaintKind::Stroke,
        }
    }

    pub fn is_painted(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn fill_rule(&self) -> Option<FillRule> {
        match self {
            Self::Fill { rule } => Some(*rule),
            _ => None,
        }
    }

    pub fn stroke(&self) -> Option<StrokeStyle> {
        match self {
            Self::Stroke(stroke) => Some(*stroke),
            _ => None,
        }
    }
}
