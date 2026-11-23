//! Artwork in the frame of an automatic mouse-bite array: who made it, what
//! it is, and the coupons and marks a fabricator and an assembler read off it.
//!
//! The frame between the array edge and the routed slots is free once the
//! tooling holes and fiducials are placed. Everything here but the row and
//! column labels stays in its outer 5 mm. The pair of rails without rail
//! tooling is the canvas: one rail names the maker and the sheet, the other
//! carries the Data Matrix and the title block. Each side is laid out as it
//! is read, the bottom mirrored, in the orientation the array is emitted in.
//!
//! Placement is one rule: pieces are placed in a fixed order, each at its home
//! on a canvas rail, else at the first spare place with room, else not at all.
//! Nothing is drawn over tooling, a fiducial, a slot or another piece.

mod shape;
mod stamps;

use sha2::{Digest, Sha256};

use super::*;
use crate::commands::board_array_auto::AUTO_MAX_GRID_COUNT;
use shape::{Fill, Layer, Mark, Shape, Stamp};
use stamps::{BAND_MM, EDGE_MM, Legend};

/// Where rail content starts and ends: clear of the corner tooling holes.
const RAIL_INSET_MM: f64 = 5.4;
/// Room between a piece and anything it must not touch.
const CLEARANCE_MM: f64 = 0.35;
/// Further room between neighbouring pieces of one rail.
const PIECE_GAP_MM: f64 = 2.0;
/// How far a row or column label may sit from its board's centre.
const LABEL_SLACK_MM: f64 = 1.5;
/// Row and column labels sit this far outboard of the routed slots.
const GRID_LABEL_INSET_MM: f64 = 0.9;
/// A letter for every column an automatic array can have; no I, which reads
/// as 1.
const COLUMN_LETTERS: &[u8] = b"ABCDEFGHJK";
const _: () = assert!(COLUMN_LETTERS.len() == AUTO_MAX_GRID_COUNT as usize);
const MAX_LABEL_CHARS: usize = 16;

/// What an order knows about an array that the board file does not. Each is
/// printed in the title block and encoded in the Data Matrix when given.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArrayLabels {
    /// The bare board's part number.
    pub part: Option<String>,
    pub order: Option<String>,
    /// The production run.
    pub run: Option<String>,
}

impl ArrayLabels {
    fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("part", &self.part),
            ("order", &self.order),
            ("run", &self.run),
        ] {
            let Some(value) = value else { continue };
            ensure!(
                !value.is_empty()
                    && value.len() <= MAX_LABEL_CHARS
                    && value.bytes().all(|byte| byte.is_ascii_graphic()),
                "{name} label must be 1 to {MAX_LABEL_CHARS} printable ASCII characters without \
                 spaces; got '{value}'"
            );
        }
        Ok(())
    }
}

/// Draw the rail artwork of `spec`, an automatic mouse-bite array made from
/// `source_xml`, on both sides.
pub(super) fn add_rail_art(
    spec: &mut BoardArraySpec,
    ipc: &Ipc2581,
    source_xml: &str,
    labels: &ArrayLabels,
    resolution: Resolution,
) -> Result<()> {
    labels.validate()?;
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let surface = crate::layers::two_sided_surface_layers(ecad)?;
    // Top to bottom, as the layer ladder numbers them.
    let stack = crate::layers::copper_order(&import_design(ipc, resolution)?)
        .into_iter()
        .map(|layer| ipc.resolve(layer).to_string())
        .collect::<Vec<_>>();
    let Some(tooling) = board_array_tooling_rails(&spec.grid) else {
        return Ok(());
    };
    let rotated = spec.packed_array_name.is_some();
    let (width, height) = if rotated {
        (spec.grid.array_height_mm, spec.grid.array_width_mm)
    } else {
        (spec.grid.array_width_mm, spec.grid.array_height_mm)
    };
    let tenth = |mm: f64| fmt_num((mm * 10.0).round() / 10.0);
    let fill = Fill(resolution);
    let hash = hex::encode_upper(&Sha256::digest(source_xml.as_bytes())[..3]);
    let mut legend = Legend {
        fields: [
            ("PART", labels.part.clone()),
            ("ORDER", labels.order.clone()),
            ("RUN", labels.run.clone()),
            ("GEN", Some(format!("PCB {}", env!("CARGO_PKG_VERSION")))),
            ("HASH", Some(hash.clone())),
        ]
        .into_iter()
        .filter_map(|(caption, value)| Some((caption, value?)))
        .collect(),
        scan: [labels.part.clone(), labels.order.clone(), Some(hash)]
            .into_iter()
            .flatten()
            .collect(),
        sheet: spec.panelization.sheet.map(|(sheet, _)| sheet.name()),
        outline: format!("{}×{}", tenth(width), tenth(height)),
        boards: spec.grid.columns * spec.grid.rows,
        top: true,
        copper_layers: stack.len(),
    };
    use pcb_ir::dialects::Side::{Bottom, Top};
    for (side, copper, mask) in [
        (Top, surface.top_copper, surface.top_soldermask),
        (Bottom, surface.bottom_copper, surface.bottom_soldermask),
    ] {
        let top = side == Top;
        legend.top = top;
        // A side without legend ink carries no artwork.
        let Some(silk) = ecad.cad_data.layers.iter().find(|layer| {
            matches!(
                layer.layer_function,
                LayerFunction::Silkscreen | LayerFunction::Legend
            ) && crate::layers::ir_side(layer.side) == side
        }) else {
            continue;
        };
        let layers = SideLayers {
            silk: ipc.resolve(silk.name),
            copper: ipc.resolve(copper),
            mask: ipc.resolve(mask),
            stack: &stack,
        };
        let view_to_packing = view_to_packing(&spec.grid, rotated, top);
        let mut panel = Panel::new(
            spec,
            top,
            layers.mask,
            view_to_packing,
            (width, height),
            // Rails along the packing frame's top and bottom stand upright
            // once the array is turned.
            (tooling == RailPair::LeftRight) != rotated,
        )?;
        panel.compose(fill, &legend)?;
        emit(
            &mut spec.generated_geometry,
            &panel.marks,
            &layers,
            view_to_packing,
        )?;
    }
    spec.content_layer_refs = content_layer_refs(
        ipc,
        &spec.generated_geometry,
        &spec.board_outline_layer_names,
    );
    Ok(())
}

/// From a side's reading frame, origin at its lower left, to the frame the
/// array is built in: the bottom is read mirrored, and an array emitted
/// turned a quarter is read as emitted.
fn view_to_packing(grid: &ArrayGrid, rotated: bool, top: bool) -> pcb_ir::geom::Affine2 {
    use pcb_ir::geom::Affine2;
    let output_width = if rotated {
        grid.array_height_mm
    } else {
        grid.array_width_mm
    };
    let view_to_output = if top {
        Affine2::IDENTITY
    } else {
        Affine2 {
            m00: -1.0,
            m02: output_width,
            ..Affine2::IDENTITY
        }
    };
    // The inverse of the quarter turn the packed array is emitted through.
    let output_to_packing = if rotated {
        Affine2 {
            m00: 0.0,
            m01: 1.0,
            m02: 0.0,
            m10: -1.0,
            m11: 0.0,
            m12: grid.array_height_mm,
        }
    } else {
        Affine2::IDENTITY
    };
    output_to_packing.concat(view_to_output)
}

/// The layers one side's marks go on.
struct SideLayers<'a> {
    silk: &'a str,
    copper: &'a str,
    mask: &'a str,
    /// Every copper layer, from the top.
    stack: &'a [String],
}

/// Turn a side's marks into generated features: copper and mask openings the
/// copper balance must keep clear of, legend ink it need not, and the areas
/// it is kept out of without a mark.
fn emit(
    geometry: &mut BoardArrayGeneratedGeometry,
    marks: &[Mark],
    layers: &SideLayers<'_>,
    view_to_packing: pcb_ir::geom::Affine2,
) -> Result<()> {
    let mut features: Vec<(&str, Vec<SetFeature>)> = Vec::new();
    let mut overlays: Vec<(&str, Vec<SetFeature>)> = Vec::new();
    for mark in marks {
        let transform = view_to_packing.concat(mark.transform);
        let (sets, layer) = match mark.layer {
            Layer::Silk => (&mut overlays, layers.silk),
            Layer::Copper => (&mut features, layers.copper),
            Layer::Mask => (&mut features, layers.mask),
            Layer::Stack(index) => (&mut features, layers.stack[index].as_str()),
            Layer::Keepout | Layer::Clear => {
                if let Shape::Area(area) = &mark.shape {
                    geometry.balance_keepouts.push(balance::BalanceKeepout {
                        layer: (mark.layer == Layer::Clear).then(|| layers.copper.to_string()),
                        area: pcb_ir::geom::attachment::transform_region(area, transform)?,
                    });
                }
                continue;
            }
        };
        let drawn = match &mark.shape {
            Shape::Line { from, to, width } => vec![solid_line_feature(
                transform.transform_point(*from),
                transform.transform_point(*to),
                *width,
            )],
            Shape::Area(area) => crate::copper_balance::ipc_region_features(
                &pcb_ir::geom::attachment::transform_region(area, transform)?,
            )?,
        };
        match sets.iter_mut().find(|(name, _)| *name == layer) {
            Some((_, set)) => set.extend(drawn),
            None => sets.push((layer, drawn)),
        }
    }
    for (layer, set) in features {
        geometry.add_layer_feature(GeneratedFeatureScope::Array, layer, set);
    }
    geometry.overlays.extend(
        overlays
            .into_iter()
            .map(|(layer, set)| (layer.to_string(), set)),
    );
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rail {
    Bottom,
    Top,
    Left,
    Right,
}

impl Rail {
    const ALL: [Rail; 4] = [Rail::Bottom, Rail::Top, Rail::Left, Rail::Right];

    fn is_horizontal(self) -> bool {
        matches!(self, Rail::Bottom | Rail::Top)
    }
}

/// One side of the array as it is read, and what is drawn on it.
struct Panel {
    top: bool,
    /// Everything drawn, in the side's reading frame.
    marks: Vec<Mark>,
    width: f64,
    height: f64,
    /// What a piece must not touch: tooling, slots, and the pieces before it.
    keepouts: Vec<BBox>,
    /// The centres of this side's global fiducials, upper left first.
    global_fiducials: Vec<Point>,
    /// The solid frame between each edge and the nearest routed slot, by
    /// `Rail as usize`.
    frame: [f64; 4],
    /// Board centres along x and along y.
    columns: Vec<f64>,
    rows: Vec<f64>,
    /// The rails without rail tooling, then the rails with it.
    canvas: [Rail; 2],
    tooling: [Rail; 2],
}

impl Panel {
    fn new(
        spec: &BoardArraySpec,
        top: bool,
        mask_layer: &str,
        view_to_packing: pcb_ir::geom::Affine2,
        (width, height): (f64, f64),
        tooling_on_sides: bool,
    ) -> Result<Self> {
        let to_view = view_to_packing
            .inverse()
            .context("rail artwork frame is singular")?;
        let view = |x: f64, y: f64| to_view.transform_point(Point::new(x, y));
        // A box of the packing frame as this side reads it, `room` wider.
        let view_box = |bbox: BBox, room: f64| bbox.transformed(to_view).expand(room);
        let around = |center: Point, radius: f64| BBox::from_point(center).expand(radius);
        let grid = &spec.grid;
        let cells = (0..grid.rows)
            .flat_map(|row| (0..grid.columns).map(move |column| (column, row)))
            .map(|(column, row)| {
                (
                    f64::from(column) * grid.pitch_x_mm,
                    f64::from(row) * grid.pitch_y_mm,
                )
            })
            .collect::<Vec<_>>();
        let mut keepouts = Vec::new();
        let mut global_fiducials = Vec::new();
        for (scope, layer_feature) in &spec.generated_geometry.layer_features {
            let origins: &[(f64, f64)] = match scope {
                GeneratedFeatureScope::Array => &[(0.0, 0.0)],
                GeneratedFeatureScope::BoardCell => &cells,
            };
            let rail = match scope {
                GeneratedFeatureScope::Array => (0.0, 0.0),
                GeneratedFeatureScope::BoardCell => {
                    (spec.edge_rail_mm.left, spec.edge_rail_mm.bottom)
                }
            };
            for feature in &layer_feature.features {
                for origin in origins {
                    let at = |x: f64, y: f64| view(x + origin.0 + rail.0, y + origin.1 + rail.1);
                    match feature {
                        SetFeature::Hole(hole) => {
                            let center = at(hole.x, hole.y);
                            // A tooling pin needs more room than a break hole.
                            let room = if hole.diameter > 1.0 { 0.7 } else { 0.4 };
                            keepouts.push(around(center, hole.diameter / 2.0 + room));
                        }
                        // Either side's: a piece read through the stackup
                        // lands on both.
                        SetFeature::Fiducial(fiducial) => {
                            let center = at(fiducial.location.x, fiducial.location.y);
                            let global = *scope == GeneratedFeatureScope::Array;
                            if global && layer_feature.layer_name == mask_layer {
                                global_fiducials.push(center);
                            }
                            keepouts.push(around(center, if global { 2.1 } else { 1.6 }));
                        }
                        _ => {}
                    }
                }
            }
        }
        global_fiducials.sort_by(|a, b| b.y.total_cmp(&a.y).then(a.x.total_cmp(&b.x)));

        let mut slots = BBox::empty();
        for cutout in &spec.profile_cutouts {
            let bbox = pcb_ir::import::ipc2581::polygon_contour(cutout).bbox;
            slots = slots.union(bbox);
            keepouts.push(view_box(bbox, 0.5));
        }
        let slots = view_box(slots, 0.0);
        // What the other side reads through the stackup.
        keepouts.extend(
            spec.generated_geometry
                .balance_keepouts
                .iter()
                .filter(|keepout| keepout.layer.is_none())
                .map(|keepout| view_box(keepout.area.bbox(), 0.0)),
        );

        let mut columns = Vec::new();
        let mut rows = Vec::new();
        for &(x, y) in &cells {
            let center = view(
                grid.margin_x_mm + x + grid.board_width_mm / 2.0,
                grid.margin_y_mm + y + grid.board_height_mm / 2.0,
            );
            columns.push(center.x);
            rows.push(center.y);
        }
        for centers in [&mut columns, &mut rows] {
            centers.sort_by(f64::total_cmp);
            centers.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        }

        let sides = [Rail::Left, Rail::Right];
        let ends = [Rail::Bottom, Rail::Top];
        let (canvas, tooling) = if tooling_on_sides {
            (ends, sides)
        } else {
            (sides, ends)
        };
        Ok(Self {
            top,
            marks: Vec::new(),
            width,
            height,
            keepouts,
            global_fiducials,
            // Indexed by `Rail as usize`.
            frame: [
                slots.min.y,
                height - slots.max.y,
                slots.min.x,
                width - slots.max.x,
            ],
            columns,
            rows,
            canvas,
            tooling,
        })
    }

    fn length(&self, rail: Rail) -> f64 {
        if rail.is_horizontal() {
            self.width
        } else {
            self.height
        }
    }

    /// A point as distance along a rail and depth in from its edge.
    fn along_and_depth(&self, rail: Rail, Point { x, y }: Point) -> (f64, f64) {
        match rail {
            Rail::Bottom => (x, y),
            Rail::Top => (x, self.height - y),
            Rail::Left => (y, x),
            Rail::Right => (y, self.width - x),
        }
    }

    /// Where a stamp lands that starts `along` a rail between depths
    /// `shallow` and `deep`. Vertical rails read upward, tops to the left.
    fn stamp_frame(
        &self,
        rail: Rail,
        along: f64,
        shallow: f64,
        deep: f64,
    ) -> pcb_ir::geom::Affine2 {
        use pcb_ir::geom::Affine2;
        let upright = |x, y| Affine2::translation(Point::new(x, y));
        let turned = |x, y| Affine2 {
            m00: 0.0,
            m01: -1.0,
            m02: x,
            m10: 1.0,
            m11: 0.0,
            m12: y,
        };
        match rail {
            Rail::Bottom => upright(along, shallow),
            Rail::Top => upright(along, self.height - deep),
            Rail::Left => turned(deep, along),
            Rail::Right => turned(self.width - shallow, along),
        }
    }

    /// The box from `s0` to `s1` along a rail between depths `d0` and `d1`.
    fn rail_box(&self, rail: Rail, (s0, s1): (f64, f64), (d0, d1): (f64, f64)) -> BBox {
        let (x0, y0, x1, y1) = match rail {
            Rail::Bottom => (s0, d0, s1, d1),
            Rail::Top => (s0, self.height - d1, s1, self.height - d0),
            Rail::Left => (d0, s0, d1, s1),
            Rail::Right => (self.width - d1, s0, self.width - d0, s1),
        };
        BBox::new(Point::new(x0, y0), Point::new(x1, y1))
    }
}

/// Where along its rail a piece goes.
#[derive(Debug, Clone, Copy)]
enum Anchor {
    /// In the first room from the rail's start.
    Start,
    /// In the last room before the rail's end.
    End,
    /// Centred in the widest room.
    Middle,
    /// Centred at this distance, or within `LABEL_SLACK_MM` of it.
    Near(f64),
}

impl Panel {
    /// The stretches of a rail between depths `shallow` and `deep` with
    /// nothing within the clearance.
    fn free(&self, rail: Rail, shallow: f64, deep: f64) -> Vec<(f64, f64)> {
        let pad = CLEARANCE_MM;
        let mut blocked = self
            .keepouts
            .iter()
            .filter_map(|keepout| {
                let (a, b) = (
                    self.along_and_depth(rail, keepout.min),
                    self.along_and_depth(rail, keepout.max),
                );
                (a.1.max(b.1) + pad > shallow && a.1.min(b.1) - pad < deep)
                    .then_some((a.0.min(b.0) - pad, a.0.max(b.0) + pad))
            })
            .collect::<Vec<_>>();
        blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
        let high = self.length(rail) - RAIL_INSET_MM;
        let mut free = Vec::new();
        let mut cursor = RAIL_INSET_MM;
        for (from, to) in blocked {
            if from > cursor {
                free.push((cursor, from.min(high)));
            }
            cursor = cursor.max(to);
        }
        if cursor < high {
            free.push((cursor, high));
        }
        free.retain(|(from, to)| to > from);
        free
    }

    /// Where `stamp` fits on a rail at `depth`: its start along the rail.
    fn find(&self, rail: Rail, depth: f64, stamp: &Stamp, anchor: Anchor) -> Option<f64> {
        let mut fits = self
            .free(rail, depth, depth + stamp.h)
            .into_iter()
            .filter(|(from, to)| to - from >= stamp.w);
        match anchor {
            Anchor::Start => fits.next().map(|(from, _)| from),
            Anchor::End => fits.next_back().map(|(_, to)| to - stamp.w),
            Anchor::Middle => fits
                .max_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)))
                .map(|(from, to)| (from + to - stamp.w) / 2.0),
            Anchor::Near(centre) => {
                let wanted = centre - stamp.w / 2.0;
                fits.map(|(from, to)| wanted.clamp(from, to - stamp.w))
                    .filter(|start| (start - wanted).abs() <= LABEL_SLACK_MM)
                    .min_by(|a, b| (a - wanted).abs().total_cmp(&(b - wanted).abs()))
            }
        }
    }

    /// Draw `stamp` on a rail at `depth` where it fits, and keep the pieces
    /// after it `gap` further along. Whether it fitted.
    fn put(&mut self, rail: Rail, depth: f64, stamp: &Stamp, anchor: Anchor, gap: f64) -> bool {
        let Some(start) = self.find(rail, depth, stamp, anchor) else {
            return false;
        };
        let frame = self.stamp_frame(rail, start, depth, depth + stamp.h);
        self.draw(stamp, frame);
        self.keepouts.push(self.rail_box(
            rail,
            (start - gap, start + stamp.w + gap),
            (depth, depth + stamp.h),
        ));
        true
    }

    fn draw(&mut self, stamp: &Stamp, frame: pcb_ir::geom::Affine2) {
        self.marks.extend(stamp.marks.iter().map(|mark| Mark {
            transform: frame.concat(mark.transform),
            ..mark.clone()
        }));
    }

    /// Draw a piece in the band at its home, else at the first spare place
    /// with room: the end of a canvas rail, then the middle of a tooling rail.
    fn place(&mut self, stamp: &Stamp, home: (Rail, Anchor)) -> bool {
        let [identity, data] = self.canvas;
        let [last, first] = self.tooling;
        [
            home,
            (data, Anchor::End),
            (identity, Anchor::End),
            (first, Anchor::Middle),
            (last, Anchor::Middle),
        ]
        .into_iter()
        .any(|(rail, anchor)| self.put(rail, band_depth(stamp), stamp, anchor, PIECE_GAP_MM))
    }

    /// The pieces in the order they claim room; one that finds none is left
    /// out.
    fn compose(&mut self, fill: Fill, legend: &Legend) -> Result<()> {
        use Anchor::{End, Middle, Start};
        self.fiducial_rings(fill)?;
        self.grid_labels(fill)?;
        let [identity, data] = self.canvas;
        self.place(&stamps::brand(fill)?, (identity, Start));
        self.place(&stamps::sheet_station(fill, legend)?, (identity, End));
        // The title block stays on its rail, with as many fields as that
        // holds: the last is dropped first.
        for count in (0..=legend.fields.len()).rev() {
            let block = stamps::title_block(fill, legend, &legend.fields[..count])?;
            if self.put(data, band_depth(&block), &block, Start, PIECE_GAP_MM) {
                break;
            }
        }
        self.place(&stamps::graduation(fill)?, (identity, Start));
        self.place(&stamps::coupons(fill, legend)?, (data, End));
        self.place(&stamps::morse(fill)?, (identity, Middle));
        self.place(&stamps::serial_blank(fill)?, (data, Start));
        Ok(())
    }

    /// A different legend ring on each global fiducial.
    fn fiducial_rings(&mut self, fill: Fill) -> Result<()> {
        for (index, center) in self.global_fiducials.clone().into_iter().enumerate() {
            let ring = stamps::fiducial_ring(fill, index)?;
            self.draw(&ring, pcb_ir::geom::Affine2::translation(center));
        }
        Ok(())
    }

    /// Column letters and row numbers beside the boards they name, on every
    /// rail. All labels of a rail share one depth: just outboard of the
    /// routed slots, or along the array edge when that places more of them.
    fn grid_labels(&mut self, fill: Fill) -> Result<()> {
        for rail in Rail::ALL {
            let centers = if rail.is_horizontal() {
                self.columns.clone()
            } else {
                self.rows.clone()
            };
            let mut labels = Vec::new();
            for (index, &center) in centers.iter().enumerate() {
                let label = if rail.is_horizontal() {
                    // A board keeps its name on both sides: the bottom is
                    // read mirrored, so its letters run right to left.
                    let column = if self.top {
                        index
                    } else {
                        centers.len() - 1 - index
                    };
                    let letter = char::from(COLUMN_LETTERS[column]);
                    stamps::grid_label(fill, &letter.to_string())?
                } else {
                    stamps::grid_label(fill, &(index + 1).to_string())?.upright()
                };
                labels.push((Anchor::Near(center), label));
            }
            let across = labels.iter().map(|(_, label)| label.h).fold(0.0, f64::max);
            let fitting = |depth| {
                labels
                    .iter()
                    .filter(|(near, label)| self.find(rail, depth, label, *near).is_some())
                    .count()
            };
            let inboard = self.frame[rail as usize] - GRID_LABEL_INSET_MM - across;
            let depth = if fitting(inboard) >= fitting(1.0) {
                inboard
            } else {
                1.0
            };
            for (near, label) in &labels {
                self.put(rail, depth, label, *near, 0.0);
            }
        }
        Ok(())
    }
}

/// The depth that centres a stamp in the band.
fn band_depth(stamp: &Stamp) -> f64 {
    EDGE_MM + (BAND_MM - EDGE_MM - stamp.h) / 2.0
}
