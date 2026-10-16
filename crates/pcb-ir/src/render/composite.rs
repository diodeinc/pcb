//! A board as it looks from one side: the physical layers a viewer sees,
//! stacked in the order they are built up.
//!
//! The drawing is ordinary artwork. Every layer is sequential paint over the
//! same tables, and every layer ends in the same cuts: all that is not board
//! material, from the space around the profile to the last drilled hole. So
//! the laminate is a sheet less the cuts, the mask is that less its
//! openings, and no artwork shows where there is no board to carry it.
//! Nothing under the outer copper can be seen from outside and none of it is
//! drawn.
//!
//! Sources lower their layers into the composite's tables and hand over
//! each side's [`Face`]; how a board is built up from them is said here
//! once, whatever the source.

use std::collections::HashSet;

use super::LayerStyle;
use crate::dialects::artwork::{self, Geometry, Object, PaintOrder, transformed_geometry};
use crate::dialects::{LayerRole, Side};
use crate::geom::{
    Affine2, BBox, ContourBuf, Diagnostics, FillRule, Paint, PathCmd, Point, Polarity,
};

pub type CompositeDocument<Meta> = artwork::Document<(), Meta>;

/// How each physical layer of the stack draws.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositeStyle {
    /// Bare laminate.
    pub substrate: LayerStyle,
    /// Copper under the mask.
    pub copper: LayerStyle,
    /// Copper the mask leaves open, in its surface finish.
    pub finish: LayerStyle,
    /// The mask itself; its opacity is how much of the copper shows through.
    pub mask: LayerStyle,
    pub legend: LayerStyle,
    /// V-score grooves across an array.
    pub score: LayerStyle,
}

const fn style(color: u32, opacity: f64) -> LayerStyle {
    LayerStyle { color, opacity }
}

impl Default for CompositeStyle {
    /// Neutral greys under a dark translucent mask, the palette tracespace's
    /// `pcb-stackup` made familiar: grey copper lightens the mask evenly, so
    /// traces read by brightness under a mask of any colour.
    fn default() -> Self {
        Self {
            substrate: style(0x666666, 1.0),
            copper: style(0xcccccc, 1.0),
            finish: style(0xcc9933, 1.0),
            mask: style(0x004200, 0.75),
            legend: style(0xffffff, 1.0),
            score: style(0x000000, 0.35),
        }
    }
}

/// Mask inks by name: the dark, slightly dull colour of the cured mask,
/// which the copper under it then lightens.
const MASK_INKS: [(&str, u32); 7] = [
    ("green", 0x004200),
    ("black", 0x0a0a0a),
    ("blue", 0x0a2260),
    ("red", 0x7a0c0c),
    ("purple", 0x3a1466),
    ("yellow", 0xc79a00),
    ("white", 0xf0f0f0),
];

impl CompositeStyle {
    /// The colour a mask ink of this name draws in, if it is a known ink. A
    /// name outside the list draws in whatever colour its source resolves
    /// it to.
    pub fn mask_ink(name: &str) -> Option<u32> {
        let name = name.trim().to_lowercase();
        MASK_INKS
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, color)| *color)
    }

    /// The default look in a side's mask and legend colours and the board's
    /// finish, each `0xRRGGBB`. What is not given keeps the default.
    pub fn with_colors(mask: Option<u32>, legend: Option<u32>, finish: Option<u32>) -> Self {
        let default = Self::default();
        let recolor = |style: LayerStyle, color: Option<u32>| LayerStyle {
            color: color.unwrap_or(style.color),
            ..style
        };
        Self {
            mask: recolor(default.mask, mask),
            legend: recolor(default.legend, legend),
            finish: recolor(default.finish, finish),
            ..default
        }
    }
}

/// A composite drawing, the style of each of its layers by index, and where
/// to look at it.
pub struct Composite<Meta> {
    pub artwork: CompositeDocument<Meta>,
    pub styles: Vec<LayerStyle>,
    /// The boards drawn and a margin around them. Artwork may reach further;
    /// the picture is of the board.
    pub viewport: BBox,
}

/// Where the board is, said once for every layer.
#[derive(Debug, Clone)]
pub struct Material<Meta> {
    /// Bounds of the profile.
    pub bounds: BBox,
    /// A sheet reaching past all the artwork.
    sheet: Object<Meta>,
    /// All of the sheet that is not board: around the profile and inside
    /// its cutouts.
    outside: Object<Meta>,
}

/// One side of the board as its source layers image it, in the composite's
/// tables and the board's frame.
#[derive(Debug, Clone)]
pub struct Face<Meta> {
    /// [`Side::Bottom`] draws turned over; any other side draws as the top.
    pub side: Side,
    pub style: CompositeStyle,
    pub copper: Vec<Object<Meta>>,
    /// Whether the side has a mask layer at all. One that opens nothing
    /// still masks the whole side; a side without one is bare.
    pub masked: bool,
    /// The mask layers' images: where the mask is open.
    pub openings: Vec<Object<Meta>>,
    pub legend: Vec<Object<Meta>>,
    /// Every slot and hole that opens onto this side.
    pub cutouts: Vec<Object<Meta>>,
}

/// What every side shares beyond the board's material: the material an
/// array's fabrication routs away, and its V-score grooves.
#[derive(Debug, Clone)]
pub struct Fabrication<Meta> {
    pub removal: Vec<Object<Meta>>,
    pub scores: Vec<Object<Meta>>,
}

impl<Meta> Default for Fabrication<Meta> {
    fn default() -> Self {
        Self {
            removal: Vec::new(),
            scores: Vec::new(),
        }
    }
}

/// Margin the view keeps around the board.
const VIEW_MARGIN_MM: f64 = 1.0;
/// Space between two sides drawn next to each other.
const SIDE_GAP_MM: f64 = 5.0;
/// How far past the board the sheet every layer is cut from reaches: further
/// than any artwork, so none of it is left to show beside the board or over
/// the side drawn next to it.
const SHEET_MARGIN_MM: f64 = 1000.0;

impl<Meta: Clone + Default> Composite<Meta> {
    pub fn new() -> Self {
        Self {
            artwork: CompositeDocument::new(),
            styles: Vec::new(),
            viewport: BBox::empty(),
        }
    }

    /// The board whose edges are the `profile` contours, read even-odd: its
    /// outline and the outlines of what is cut out of it. `None` when the
    /// profile bounds nothing.
    pub fn material(&mut self, profile: Vec<ContourBuf>) -> Option<Material<Meta>> {
        let bounds = profile
            .iter()
            .map(|contour| contour.bbox)
            .fold(BBox::empty(), BBox::union);
        if bounds.is_empty() {
            return None;
        }
        let BBox { min, max } = bounds.expand(SHEET_MARGIN_MM);
        let sheet = ContourBuf::new(vec![
            PathCmd::move_to(min),
            PathCmd::line_to(Point::new(max.x, min.y)),
            PathCmd::line_to(max),
            PathCmd::line_to(Point::new(min.x, max.y)),
            PathCmd::close(),
        ]);
        // Even-odd, the profile inside the sheet is a hole in it and a cutout
        // inside the profile is filled again.
        let outside = std::iter::once(sheet.clone()).chain(profile);
        Some(Material {
            bounds,
            sheet: self.region(Polarity::Dark, [sheet]),
            outside: self.region(Polarity::Clear, outside),
        })
    }

    /// An even-odd region over `contours`, in the composite's tables.
    pub fn region(
        &mut self,
        polarity: Polarity,
        contours: impl IntoIterator<Item = ContourBuf>,
    ) -> Object<Meta> {
        let fill = Paint::Fill {
            rule: FillRule::EvenOdd,
        };
        let path = self.artwork.push_path(fill, contours);
        Object::new(polarity, Geometry::Region { path })
    }

    /// Draw `faces` of the board `material` makes, left to right, and frame
    /// them. The bottom is drawn as it looks turned over about the board's
    /// vertical axis.
    pub fn draw(
        &mut self,
        material: &Material<Meta>,
        fabrication: &Fabrication<Meta>,
        faces: Vec<Face<Meta>>,
    ) -> Result<(), Diagnostics> {
        let bounds = material.bounds;
        let turned_over = Affine2 {
            m00: -1.0,
            m02: bounds.min.x + bounds.max.x,
            ..Affine2::IDENTITY
        };
        for (index, face) in faces.into_iter().enumerate() {
            let across = Point::new(index as f64 * (bounds.width() + SIDE_GAP_MM), 0.0);
            let view = match face.side {
                Side::Bottom => turned_over,
                _ => Affine2::IDENTITY,
            };
            self.stack(
                Affine2::translation(across).concat(view),
                material,
                fabrication,
                face,
            );
            let placed = BBox::new(bounds.min + across, bounds.max + across);
            self.viewport = self.viewport.union(placed);
        }
        self.viewport = self.viewport.expand(VIEW_MARGIN_MM);

        // Sources lowering several layers repeat their diagnostics.
        let mut seen = HashSet::new();
        self.artwork
            .diagnostics
            .retain(|diagnostic| seen.insert(diagnostic.message.clone()));
        artwork::normalize_bounds(&mut self.artwork);
        self.artwork.validate()
    }

    /// Stack one side's six layers under `placement`. Every layer is its
    /// paint, then what the mask opens where that clips it, then the cuts.
    fn stack(
        &mut self,
        placement: Affine2,
        material: &Material<Meta>,
        fabrication: &Fabrication<Meta>,
        face: Face<Meta>,
    ) {
        let mut place = |objects: Vec<Object<Meta>>, polarity: Option<Polarity>| {
            objects
                .into_iter()
                .map(|object| Object {
                    polarity: polarity.unwrap_or(object.polarity),
                    order: PaintOrder::default(),
                    geometry: transformed_geometry(&mut self.artwork, object.geometry, placement),
                    ..object
                })
                .collect::<Vec<_>>()
        };
        let sheet = place(vec![material.sheet.clone()], None);
        let copper = place(face.copper, None);
        let legend = place(face.legend, None);
        let scores = place(fabrication.scores.clone(), None);
        // A mask layer images its openings, so they clear whatever they
        // paint.
        let openings = place(face.openings, None)
            .into_iter()
            .map(|opening| Object {
                polarity: Polarity::Clear.compose(opening.polarity),
                ..opening
            })
            .collect::<Vec<_>>();
        // A cut clears on every layer, painted or not. Left a final cutout
        // it would image as itself wherever a layer paints nothing under it.
        let cuts = std::iter::once(material.outside.clone())
            .chain(fabrication.removal.iter().cloned())
            .chain(face.cutouts)
            .collect();
        let cuts = place(cuts, Some(Polarity::Clear));
        // A mask layer lays the mask over the whole sheet, however little it
        // opens, and covers the copper under it. A side without one is bare.
        let (mask, covered) = if face.masked {
            (sheet.clone(), copper.clone())
        } else {
            (Vec::new(), Vec::new())
        };

        self.layer("Substrate", face.style.substrate, [&sheet, &cuts]);
        // All the copper in its finish, then what the mask covers over it:
        // the finish is left showing exactly where the mask opens.
        self.layer("Finish", face.style.finish, [&copper, &cuts]);
        self.layer("Copper", face.style.copper, [&covered, &openings, &cuts]);
        self.layer("Mask", face.style.mask, [&mask, &openings, &cuts]);
        // Mask openings cut the legend, as a fabricator clips it off the pads.
        self.layer("Legend", face.style.legend, [&legend, &openings, &cuts]);
        self.layer("Score", face.style.score, [&scores, &cuts]);
    }

    /// A layer painting `objects` in order. Every layer is drawn material of
    /// no fabrication role: what it looks like is its style's to say.
    fn layer<const N: usize>(
        &mut self,
        name: &str,
        style: LayerStyle,
        objects: [&Vec<Object<Meta>>; N],
    ) {
        self.styles.push(style);
        let layer =
            self.artwork
                .push_layer(artwork::Layer::new(name, LayerRole::Other, Side::None));
        for object in objects.into_iter().flatten() {
            self.artwork.push_object(layer, object.clone());
        }
    }
}

impl<Meta: Clone + Default> Default for Composite<Meta> {
    fn default() -> Self {
        Self::new()
    }
}
