//! Drill symbols: one mark per tool, told apart by shape alone.

use pcb_ir::geom::{ContourBuf, PathCmd, Point};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Figure {
    None,
    Circle,
    Square,
    Diamond,
    TriangleUp,
    TriangleDown,
    Hexagon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    Plus,
    Cross,
    Star,
    Dot,
    Solid,
}

/// Lightest to heaviest: the tools with the most holes take the lightest.
const SYMBOLS: [(Figure, Mark); 34] = [
    (Figure::None, Mark::Plus),
    (Figure::None, Mark::Cross),
    (Figure::Circle, Mark::None),
    (Figure::Square, Mark::None),
    (Figure::TriangleUp, Mark::None),
    (Figure::Diamond, Mark::None),
    (Figure::TriangleDown, Mark::None),
    (Figure::Circle, Mark::Plus),
    (Figure::Square, Mark::Cross),
    (Figure::Diamond, Mark::Plus),
    (Figure::Circle, Mark::Cross),
    (Figure::Square, Mark::Plus),
    (Figure::None, Mark::Star),
    (Figure::Circle, Mark::Dot),
    (Figure::Square, Mark::Dot),
    (Figure::Diamond, Mark::Dot),
    (Figure::TriangleUp, Mark::Dot),
    (Figure::TriangleDown, Mark::Dot),
    (Figure::Circle, Mark::Solid),
    (Figure::Square, Mark::Solid),
    (Figure::Diamond, Mark::Solid),
    (Figure::TriangleUp, Mark::Solid),
    (Figure::TriangleDown, Mark::Solid),
    // A hexagon is hard to tell from a circle at symbol size, so it comes last.
    (Figure::Hexagon, Mark::Dot),
    (Figure::Hexagon, Mark::Solid),
    (Figure::Hexagon, Mark::Plus),
    (Figure::Hexagon, Mark::Cross),
    (Figure::Circle, Mark::Star),
    (Figure::Square, Mark::Star),
    (Figure::Diamond, Mark::Cross),
    (Figure::Diamond, Mark::Star),
    (Figure::Hexagon, Mark::Star),
    (Figure::TriangleUp, Mark::Plus),
    (Figure::Hexagon, Mark::None),
];

/// Line weight of a symbol, as a share of its size.
const WEIGHT: f64 = 0.11;

/// Centred on the origin, non-zero fill; repeats a half larger past the last.
pub fn symbol(index: usize, size: f64) -> ContourBuf {
    let (figure, mark) = SYMBOLS[index % SYMBOLS.len()];
    let size = size * (1.0 + 0.5 * (index / SYMBOLS.len()) as f64);
    let mut cmds = Vec::new();
    let reach = match figure {
        Figure::None => 0.5,
        _ => {
            let (radius, inner) = (figure.radius(), figure.inset());
            figure.outline(&mut cmds, radius * size, false);
            if mark != Mark::Solid {
                figure.outline(&mut cmds, inner * size, true);
            }
            figure.reach()
        }
    };
    let bar = |cmds: &mut Vec<PathCmd>, degrees: f64, reach: f64| {
        let (sin, cos) = degrees.to_radians().sin_cos();
        let along = Point::new(cos, sin) * (reach * size);
        let across = Point::new(-sin, cos) * (WEIGHT / 2.0 * size);
        polygon(
            cmds,
            &[
                -along - across,
                along - across,
                along + across,
                -along + across,
            ],
        );
    };
    let (strokes, reach): (&[f64], f64) = match mark {
        Mark::Plus => (&[0.0, 90.0], reach),
        Mark::Cross => (&[45.0, 135.0], figure.diagonal_reach()),
        Mark::Star => (&[0.0, 60.0, 120.0], reach.min(figure.diagonal_reach())),
        Mark::None | Mark::Solid | Mark::Dot => (&[], reach),
    };
    for &angle in strokes {
        bar(&mut cmds, angle, reach);
    }
    if mark == Mark::Dot {
        circle(&mut cmds, 0.14 * size, false);
    }
    ContourBuf::new(cmds)
}

impl Figure {
    /// Circumradius, chosen so every figure looks as large as the others.
    fn radius(self) -> f64 {
        match self {
            Self::None | Self::Circle => 0.5,
            Self::Square => 0.62,
            Self::Diamond => 0.6,
            Self::TriangleUp | Self::TriangleDown => 0.64,
            Self::Hexagon => 0.54,
        }
    }

    /// Vertices and the angle of the first, for the straight-sided figures.
    fn vertices(self) -> Option<(u32, f64)> {
        match self {
            Self::None | Self::Circle => None,
            Self::Square => Some((4, 45.0)),
            Self::Diamond => Some((4, 0.0)),
            Self::TriangleUp => Some((3, 90.0)),
            Self::TriangleDown => Some((3, 270.0)),
            Self::Hexagon => Some((6, 0.0)),
        }
    }

    /// Circumradius of the figure's inner edge, a line weight inside it.
    fn inset(self) -> f64 {
        match self.vertices() {
            None => self.radius() - WEIGHT,
            Some((count, _)) => {
                let apothem = (std::f64::consts::PI / f64::from(count)).cos();
                (self.radius() * apothem - WEIGHT) / apothem
            }
        }
    }

    /// How far a horizontal or vertical stroke runs from the centre.
    fn reach(self) -> f64 {
        match self {
            Self::None | Self::Circle => 0.5,
            Self::Square => self.radius() * std::f64::consts::FRAC_1_SQRT_2,
            Self::Diamond => self.radius(),
            // A triangle's centre is nearer its base than its apex.
            Self::TriangleUp | Self::TriangleDown => self.radius() * 0.45,
            Self::Hexagon => self.radius() * 0.86,
        }
    }

    fn diagonal_reach(self) -> f64 {
        match self {
            Self::None | Self::Circle => 0.5,
            Self::Square => self.radius(),
            Self::Diamond => self.radius() * std::f64::consts::FRAC_1_SQRT_2,
            Self::TriangleUp | Self::TriangleDown => self.radius() * 0.45,
            Self::Hexagon => self.radius() * 0.86,
        }
    }

    fn outline(self, cmds: &mut Vec<PathCmd>, radius: f64, clockwise: bool) {
        let Some((count, first)) = self.vertices() else {
            return circle(cmds, radius, clockwise);
        };
        let mut points = (0..count)
            .map(|vertex| {
                let angle = (first + 360.0 * f64::from(vertex) / f64::from(count)).to_radians();
                Point::new(angle.cos(), angle.sin()) * radius
            })
            .collect::<Vec<_>>();
        if clockwise {
            points.reverse();
        }
        polygon(cmds, &points);
    }
}

fn polygon(cmds: &mut Vec<PathCmd>, points: &[Point]) {
    cmds.push(PathCmd::move_to(points[0]));
    cmds.extend(points[1..].iter().map(|&point| PathCmd::line_to(point)));
    cmds.push(PathCmd::close());
}

fn circle(cmds: &mut Vec<PathCmd>, radius: f64, clockwise: bool) {
    let (east, west) = (Point::new(radius, 0.0), Point::new(-radius, 0.0));
    cmds.extend([
        PathCmd::move_to(east),
        PathCmd::arc_to(west, Point::ZERO, clockwise),
        PathCmd::arc_to(east, Point::ZERO, clockwise),
        PathCmd::close(),
    ]);
}
