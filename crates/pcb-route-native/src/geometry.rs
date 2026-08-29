//! Extracts board geometry (footprints, pads, board outline) from a parsed
//! `.kicad_pcb` file. This module does no rasterization or pathfinding — see
//! [`crate::grid`] for that.

use pcb_sexpr::{Sexpr, find_all_child_lists, find_child_list, parse as parse_sexpr};

#[derive(Debug, thiserror::Error)]
pub enum GeometryError {
    #[error("failed to parse board s-expression: {0}")]
    Parse(String),
    #[error("expected top-level (kicad_pcb ...) list")]
    NotAKicadPcb,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Rotate this point (treated as an offset from the origin) by `degrees`
    /// — matching KiCad's `(at x y rot)` convention — then translate by
    /// `origin`.
    ///
    /// TODO(review): sign convention is implemented and self-consistently
    /// tested here, but not yet cross-checked against a real KiCad-exported
    /// board with a known rotated part. Verify before merging.
    fn rotated_and_translated(self, origin: Point, degrees: f64) -> Point {
        let radians = degrees.to_radians();
        let (sin, cos) = radians.sin_cos();
        let rx = self.x * cos - self.y * sin;
        let ry = self.x * sin + self.y * cos;
        Point::new(origin.x + rx, origin.y + ry)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadShape {
    Circle,
    Rect,
    Oval,
    RoundRect,
    Other,
}

impl PadShape {
    fn from_str(s: &str) -> Self {
        match s {
            "circle" => PadShape::Circle,
            "rect" => PadShape::Rect,
            "oval" => PadShape::Oval,
            "roundrect" => PadShape::RoundRect,
            _ => PadShape::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pad {
    pub number: String,
    /// Absolute board position (mm), after applying the parent footprint's
    /// `(at ...)` pose to the pad's own local `(at ...)`.
    pub position: Point,
    pub shape: PadShape,
    /// (width, height) in mm, in the pad's unrotated local frame.
    pub size: (f64, f64),
    pub drill: Option<f64>,
    pub layers: Vec<String>,
    pub net_name: Option<String>,
}

impl Pad {
    /// Radius (mm) of the smallest circle centered on the pad that fully
    /// contains it at any rotation. Deliberately conservative: grid
    /// rasterization only needs "is this cell inside the obstacle", not an
    /// exact rotated-rectangle polygon, so we don't need to track the pad's
    /// own local rotation angle at all for this PR's scope.
    pub fn bounding_radius(&self) -> f64 {
        let (w, h) = self.size;
        (w * w + h * h).sqrt() / 2.0
    }

    pub fn is_on_layer(&self, layer: &str) -> bool {
        self.layers.iter().any(|l| l == layer || l == "*.Cu")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Footprint {
    pub reference: Option<String>,
    pub fpid: Option<String>,
    pub position: Point,
    pub rotation: f64,
    pub layer: String,
    pub pads: Vec<Pad>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoardOutline {
    pub min: Point,
    pub max: Point,
}

impl BoardOutline {
    pub fn width(&self) -> f64 {
        self.max.x - self.min.x
    }
    pub fn height(&self) -> f64 {
        self.max.y - self.min.y
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BoardGeometry {
    pub footprints: Vec<Footprint>,
    /// `None` when the board has no `Edge.Cuts` graphics at all; callers
    /// should fall back to a bounding box over placed footprints (see
    /// [`crate::grid::build_grid`]).
    pub outline: Option<BoardOutline>,
}

/// Parse a `.kicad_pcb` file's text into board geometry.
pub fn parse_board(text: &str) -> Result<BoardGeometry, GeometryError> {
    let root = parse_sexpr(text).map_err(|e| GeometryError::Parse(e.to_string()))?;
    let items = root.as_list().ok_or(GeometryError::NotAKicadPcb)?;
    if items.first().and_then(Sexpr::as_sym) != Some("kicad_pcb") {
        return Err(GeometryError::NotAKicadPcb);
    }

    let footprints = find_all_child_lists(items, "footprint")
        .into_iter()
        .filter_map(parse_footprint)
        .collect();

    let outline = parse_edge_cuts_outline(items);

    Ok(BoardGeometry {
        footprints,
        outline,
    })
}

fn number(node: &Sexpr) -> Option<f64> {
    node.as_float().or_else(|| node.as_int().map(|v| v as f64))
}

/// Parse `(at x y [rot])` into (position, rotation_degrees). `rot` defaults to 0.
fn parse_at(items: &[Sexpr]) -> Option<(Point, f64)> {
    let at = find_child_list(items, "at")?;
    let x = number(at.get(1)?)?;
    let y = number(at.get(2)?)?;
    let rot = at.get(3).and_then(number).unwrap_or(0.0);
    Some((Point::new(x, y), rot))
}

fn parse_size(items: &[Sexpr]) -> Option<(f64, f64)> {
    let size = find_child_list(items, "size")?;
    let w = number(size.get(1)?)?;
    let h = number(size.get(2)?)?;
    Some((w, h))
}

fn parse_layers(items: &[Sexpr]) -> Vec<String> {
    find_child_list(items, "layers")
        .map(|layers| {
            layers[1..]
                .iter()
                .filter_map(Sexpr::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a single `(pad "1" smd roundrect (at ...) (size ...) ...)` list.
/// `position` is left in the pad's *local* (footprint-relative) frame here;
/// [`parse_footprint`] resolves it to absolute board coordinates.
fn parse_pad(items: &[Sexpr]) -> Option<Pad> {
    if items.first().and_then(Sexpr::as_sym) != Some("pad") {
        return None;
    }
    let number_str = items.get(1)?.as_str()?.to_string();
    // items[2] = mount type (smd/thru_hole/np_thru_hole/connect); items[3] = pad shape.
    let shape = items
        .get(3)
        .and_then(Sexpr::as_sym)
        .map(PadShape::from_str)
        .unwrap_or(PadShape::Other);
    let (local_position, _local_rotation) = parse_at(items)?;
    let size = parse_size(items)?;
    let drill = find_child_list(items, "drill")
        .and_then(|d| d.get(1))
        .and_then(number);
    let layers = parse_layers(items);
    let net_name = find_child_list(items, "net")
        .and_then(|n| n.get(2))
        .and_then(Sexpr::as_str)
        .map(str::to_string);

    Some(Pad {
        number: number_str,
        position: local_position,
        shape,
        size,
        drill,
        layers,
        net_name,
    })
}

fn parse_footprint(items: &[Sexpr]) -> Option<Footprint> {
    let fpid = items.get(1).and_then(Sexpr::as_str).map(str::to_string);
    let layer = find_child_list(items, "layer")
        .and_then(|l| l.get(1))
        .and_then(Sexpr::as_str)
        .unwrap_or("F.Cu")
        .to_string();
    let (position, rotation) = parse_at(items)?;
    let reference = find_all_child_lists(items, "property")
        .into_iter()
        .find(|p| p.get(1).and_then(Sexpr::as_str) == Some("Reference"))
        .and_then(|p| p.get(2))
        .and_then(Sexpr::as_str)
        .map(str::to_string);

    let pads = find_all_child_lists(items, "pad")
        .into_iter()
        .filter_map(|pad_items| {
            let mut pad = parse_pad(pad_items)?;
            pad.position = pad.position.rotated_and_translated(position, rotation);
            Some(pad)
        })
        .collect();

    Some(Footprint {
        reference,
        fpid,
        position,
        rotation,
        layer,
        pads,
    })
}

fn is_edge_cuts(graphic: &[Sexpr]) -> bool {
    find_child_list(graphic, "layer")
        .and_then(|l| l.get(1))
        .and_then(Sexpr::as_str)
        == Some("Edge.Cuts")
}

/// Bounding box over every `Edge.Cuts` graphic (`gr_line`, `gr_rect`, `gr_poly`,
/// `gr_arc`) at the board's top level. This is an approximation for non-rectangular
/// outlines (e.g. a rounded or L-shaped board) — full polygon outline support is
/// out of scope for this PR.
fn parse_edge_cuts_outline(items: &[Sexpr]) -> Option<BoardOutline> {
    let mut min = Point::new(f64::INFINITY, f64::INFINITY);
    let mut max = Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut found = false;

    for name in ["gr_line", "gr_rect", "gr_poly", "gr_arc"] {
        for graphic in find_all_child_lists(items, name) {
            if !is_edge_cuts(graphic) {
                continue;
            }
            for point_field in ["start", "end", "center", "mid"] {
                if let Some(p) = find_child_list(graphic, point_field)
                    && let (Some(x), Some(y)) =
                        (p.get(1).and_then(number), p.get(2).and_then(number))
                {
                    min.x = min.x.min(x);
                    min.y = min.y.min(y);
                    max.x = max.x.max(x);
                    max.y = max.y.max(y);
                    found = true;
                }
            }
            if let Some(pts) = find_child_list(graphic, "pts") {
                for xy in find_all_child_lists(pts, "xy") {
                    if let (Some(x), Some(y)) =
                        (xy.get(1).and_then(number), xy.get(2).and_then(number))
                    {
                        min.x = min.x.min(x);
                        min.y = min.y.min(y);
                        max.x = max.x.max(x);
                        max.y = max.y.max(y);
                        found = true;
                    }
                }
            }
        }
    }

    if found {
        Some(BoardOutline { min, max })
    } else {
        None
    }
}
