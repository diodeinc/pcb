//! What rail artwork is drawn with: marks on manufacturing layers, grouped
//! into stamps that a rail places as one piece.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use anyhow::Result;
use pcb_ir::geom::{Affine2, ContourSet, FillRule, Point, Resolution, Ring};

/// How far a drawn arc may leave the circle it stands for.
const ARC_ERROR_MM: f64 = 0.003;

/// The layer a mark is made on, relative to the side being drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layer {
    /// Legend ink.
    Silk,
    /// The side's outer copper.
    Copper,
    /// A copper layer of the stackup, counted from the top one.
    Stack(usize),
    /// A solder-mask opening.
    Mask,
    /// No mark: balance copper stays out of this area on every layer.
    Keepout,
    /// No mark: balance copper stays out of this area on the side's outer
    /// copper, so what is printed here has one background.
    Clear,
}

#[derive(Debug, Clone)]
pub(super) enum Shape {
    /// A line drawn with a round pen.
    Line {
        from: Point,
        to: Point,
        width: f64,
    },
    Area(ContourSet),
}

#[derive(Debug, Clone)]
pub(super) struct Mark {
    /// Where the shape's own coordinates land.
    pub transform: Affine2,
    pub layer: Layer,
    pub shape: Shape,
}

/// Marks in a box of `w` along the rail by `h` across it, from the box's
/// lower-left corner.
#[derive(Debug, Clone, Default)]
pub(super) struct Stamp {
    pub w: f64,
    pub h: f64,
    pub marks: Vec<Mark>,
}

impl Stamp {
    pub fn new(w: f64, h: f64) -> Self {
        Self {
            w,
            h,
            marks: Vec::new(),
        }
    }

    pub fn line(&mut self, layer: Layer, from: (f64, f64), to: (f64, f64), width: f64) {
        self.marks.push(Mark {
            transform: Affine2::IDENTITY,
            layer,
            shape: Shape::Line {
                from: Point::new(from.0, from.1),
                to: Point::new(to.0, to.1),
                width,
            },
        });
    }

    /// The outline of the box `u0..u1` by `v0..v1`.
    pub fn frame(&mut self, layer: Layer, (u0, v0, u1, v1): (f64, f64, f64, f64), width: f64) {
        for (from, to) in [
            ((u0, v0), (u1, v0)),
            ((u1, v0), (u1, v1)),
            ((u1, v1), (u0, v1)),
            ((u0, v1), (u0, v0)),
        ] {
            self.line(layer, from, to, width);
        }
    }

    pub fn area(&mut self, layer: Layer, area: ContourSet) {
        if !area.is_empty() {
            self.marks.push(Mark {
                transform: Affine2::IDENTITY,
                layer,
                shape: Shape::Area(area),
            });
        }
    }

    /// Draw `child` with its corner at `(u, v)`.
    pub fn place(&mut self, child: &Stamp, u: f64, v: f64) {
        self.place_through(child, Affine2::translation(Point::new(u, v)));
    }

    pub fn place_through(&mut self, child: &Stamp, transform: Affine2) {
        self.marks.extend(child.marks.iter().map(|mark| Mark {
            transform: transform.concat(mark.transform),
            ..mark.clone()
        }));
    }

    /// The stamp turned a quarter clockwise, so what read along a vertical
    /// rail reads across it.
    pub fn upright(&self) -> Stamp {
        let mut turned = Stamp::new(self.h, self.w);
        turned.place_through(
            self,
            Affine2 {
                m00: 0.0,
                m01: 1.0,
                m02: 0.0,
                m10: -1.0,
                m11: 0.0,
                m12: self.w,
            },
        );
        turned
    }
}

/// Stamps side by side with `gap` between them, centred across the row.
pub(super) fn row(items: &[Stamp], gap: f64) -> Stamp {
    let w = items.iter().map(|item| item.w).sum::<f64>() + gap * (items.len().max(1) - 1) as f64;
    let h = items.iter().map(|item| item.h).fold(0.0, f64::max);
    let mut out = Stamp::new(w, h);
    let mut u = 0.0;
    for item in items {
        out.place(item, u, (h - item.h) / 2.0);
        u += item.w + gap;
    }
    out
}

/// Stamps one over the next from the top, left edges aligned.
pub(super) fn stack(items: &[Stamp], gap: f64) -> Stamp {
    let w = items.iter().map(|item| item.w).fold(0.0, f64::max);
    let h = items.iter().map(|item| item.h).sum::<f64>() + gap * (items.len().max(1) - 1) as f64;
    let mut out = Stamp::new(w, h);
    let mut v = h;
    for item in items {
        v -= item.h;
        out.place(item, 0.0, v);
        v -= gap;
    }
    out
}

/// Builds filled areas at one resolution.
#[derive(Debug, Clone, Copy)]
pub(super) struct Fill(pub Resolution);

impl Fill {
    /// The union of counter-clockwise rings.
    pub fn rings(self, rings: Vec<Ring>) -> Result<ContourSet> {
        Ok(ContourSet::from_rings(rings, FillRule::NonZero, self.0)?)
    }

    pub fn rounded_rect(self, bounds: (f64, f64, f64, f64), radius: f64) -> Result<ContourSet> {
        self.rings(vec![rounded_rect_ring(bounds, radius)])
    }

    pub fn disc(self, center: (f64, f64), radius: f64) -> Result<ContourSet> {
        self.rings(vec![arc_ring(center, radius, 0.0, TAU)])
    }

    /// What a round pen of `width` inks along `lines`.
    pub fn strokes(self, lines: &[[(f64, f64); 2]], width: f64) -> Result<ContourSet> {
        self.rings(
            lines
                .iter()
                .map(|&[from, to]| capsule_ring(from, to, width / 2.0))
                .collect(),
        )
    }
}

pub(super) fn rect_ring((u0, v0, u1, v1): (f64, f64, f64, f64)) -> Ring {
    vec![[u0, v0], [u1, v0], [u1, v1], [u0, v1]]
}

fn rounded_rect_ring(bounds: (f64, f64, f64, f64), radius: f64) -> Ring {
    let (u0, v0, u1, v1) = bounds;
    let r = radius.min((u1 - u0) / 2.0).min((v1 - v0) / 2.0);
    if r <= 0.0 {
        return rect_ring(bounds);
    }
    [
        ((u1 - r, v0 + r), -FRAC_PI_2),
        ((u1 - r, v1 - r), 0.0),
        ((u0 + r, v1 - r), FRAC_PI_2),
        ((u0 + r, v0 + r), PI),
    ]
    .into_iter()
    .flat_map(|(center, from)| arc_points(center, r, from, from + FRAC_PI_2))
    .collect()
}

/// A line's sweep under a round pen of `radius`.
fn capsule_ring(from: (f64, f64), to: (f64, f64), radius: f64) -> Ring {
    let (du, dv) = (to.0 - from.0, to.1 - from.1);
    if du == 0.0 && dv == 0.0 {
        return arc_ring(from, radius, 0.0, TAU);
    }
    let heading = dv.atan2(du);
    let mut ring = arc_points(to, radius, heading - FRAC_PI_2, heading + FRAC_PI_2);
    ring.extend(arc_points(
        from,
        radius,
        heading + FRAC_PI_2,
        heading + 3.0 * FRAC_PI_2,
    ));
    ring
}

/// A closed counter-clockwise arc: a whole turn leaves out its repeated end.
fn arc_ring(center: (f64, f64), radius: f64, from: f64, to: f64) -> Ring {
    let mut ring = arc_points(center, radius, from, to);
    ring.pop();
    ring
}

/// Points along a counter-clockwise arc, both ends included.
pub(super) fn arc_points(center: (f64, f64), radius: f64, from: f64, to: f64) -> Vec<[f64; 2]> {
    let step = 2.0 * (1.0 - ARC_ERROR_MM / radius).clamp(0.0, 1.0).acos();
    let count = ((to - from) / step.max(1e-3)).ceil().max(2.0) as usize;
    (0..=count)
        .map(|index| {
            let (sin, cos) = (from + (to - from) * index as f64 / count as f64).sin_cos();
            [center.0 + radius * cos, center.1 + radius * sin]
        })
        .collect()
}

/// The smallest pen JLCPCB prints legend text with, and the smallest it
/// leaves open inside a printed block.
const MIN_PEN_MM: f64 = 0.15;
const MIN_KNOCKOUT_PEN_MM: f64 = 0.20;

/// The pen for capitals `cap` tall: a sixth of the height for values, KiCad's
/// eighth for captions.
pub(super) fn pen(cap: f64, bold: bool) -> f64 {
    MIN_PEN_MM.max(cap / if bold { 6.0 } else { 8.0 })
}

/// A line of KiCad stroke-font text: pen centrelines placed so the ink of
/// its capitals fills `0..w` by `0..cap`.
#[derive(Debug, Clone)]
pub(super) struct Text {
    pub lines: Vec<[(f64, f64); 2]>,
    pub w: f64,
    pub cap: f64,
    pub pen: f64,
}

impl Text {
    /// `track` is extra space after every glyph.
    pub fn new(text: &str, cap: f64, pen: f64, track: f64) -> Self {
        // Capitals stand 21 font units tall, one unit above the font's origin.
        let scale = cap - pen;
        let mut cursor = 0.0;
        let mut lines = Vec::new();
        for c in text.chars() {
            let glyph = newstroke::glyph(c);
            for stroke in glyph.strokes() {
                let points = stroke
                    .map(|(x, y)| (cursor + x * scale, (1.0 / 21.0 - y) * scale))
                    .collect::<Vec<_>>();
                lines.extend(points.windows(2).map(|pair| [pair[0], pair[1]]));
            }
            cursor += glyph.advance * scale + track;
        }
        let (low, high) = lines
            .iter()
            .flatten()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(low, high), point| {
                (low.min(point.0), high.max(point.0))
            });
        let low = if lines.is_empty() { 0.0 } else { low };
        let shift = |(u, v): (f64, f64)| (u - low + pen / 2.0, v + pen / 2.0);
        Self {
            w: if lines.is_empty() {
                0.0
            } else {
                high - low + pen
            },
            lines: lines
                .into_iter()
                .map(|[from, to]| [shift(from), shift(to)])
                .collect(),
            cap,
            pen,
        }
    }

    pub fn bold(text: &str, cap: f64, track: f64) -> Self {
        Self::new(text, cap, pen(cap, true), track)
    }

    /// The text drawn on `layer`.
    pub fn stamp(&self, layer: Layer) -> Stamp {
        let mut stamp = Stamp::new(self.w, self.cap);
        for [from, to] in &self.lines {
            stamp.line(layer, *from, *to, self.pen);
        }
        stamp
    }
}

/// Bold legend text.
pub(super) fn silk_text(text: &str, cap: f64) -> Stamp {
    Text::bold(text, cap, 0.0).stamp(Layer::Silk)
}

/// Text left open in a printed block: JLCPCB's hollow characters.
pub(super) fn chip(
    fill: Fill,
    text: &str,
    cap: f64,
    (pad_u, pad_v): (f64, f64),
    radius: f64,
) -> Result<Stamp> {
    let text = Text::new(text, cap, MIN_KNOCKOUT_PEN_MM.max(cap / 6.0), 0.0);
    let mut stamp = Stamp::new(text.w + 2.0 * pad_u, cap + 2.0 * pad_v);
    let lines = text
        .lines
        .iter()
        .map(|line| line.map(|(u, v)| (u + pad_u, v + pad_v)))
        .collect::<Vec<_>>();
    stamp.area(
        Layer::Silk,
        fill.rounded_rect((0.0, 0.0, stamp.w, stamp.h), radius)?
            .difference(&fill.strokes(&lines, text.pen)?)?,
    );
    Ok(stamp)
}
