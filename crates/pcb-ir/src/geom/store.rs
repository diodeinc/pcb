//! Flat arena storage for path geometry.
//!
//! Dialect documents store geometry as structure-of-arrays: styled [`Path`]s
//! reference a [`Span`] of [`Contour`]s, which reference a [`Span`] of
//! [`PathCmd`]s. [`PathArena`] owns the three arrays and every push/read/copy
//! operation over them, so dialects and consumers never index by hand.

use std::ops::Range;

use crate::geom::affine::Affine2;
use crate::geom::bbox::BBox;
use crate::geom::path::{ContourBuf, PathCmd, contour_bbox, transform_cmds, validate_cmd_points};
use crate::geom::style::{FillRule, Paint, StrokeStyle};

/// A half-open range of `u32` indices into one of a document's flat arenas.
///
/// Which arena a span indexes is determined by the field that holds it
/// (`layer.objects` indexes `doc.objects`, `path.contours` indexes
/// `arena.contours`, ...). Read through [`Span::slice`]:
///
/// ```ignore
/// for feature in layer.features.slice(&doc.features) { ... }
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: u32,
    pub count: u32,
}

impl Span {
    pub const EMPTY: Self = Self { start: 0, count: 0 };

    pub fn new(start: u32, count: u32) -> Self {
        Self { start, count }
    }

    pub fn single(index: u32) -> Self {
        Self {
            start: index,
            count: 1,
        }
    }

    pub fn len(self) -> usize {
        self.count as usize
    }

    pub fn is_empty(self) -> bool {
        self.count == 0
    }

    pub fn end(self) -> u32 {
        self.start + self.count
    }

    pub fn range(self) -> Range<usize> {
        self.start as usize..self.end() as usize
    }

    pub fn indices(self) -> Range<u32> {
        self.start..self.end()
    }

    pub fn slice<T>(self, items: &[T]) -> &[T] {
        &items[self.range()]
    }

    pub fn slice_mut<T>(self, items: &mut [T]) -> &mut [T] {
        &mut items[self.range()]
    }

    pub(crate) fn validate(self, name: &str, index: usize, len: usize) -> Result<(), String> {
        let end = self.start as usize + self.count as usize;
        if self.start as usize > len || end > len {
            Err(format!(
                "{name} range for item {index} is out of bounds: {}..{} of {len}",
                self.start,
                self.end(),
            ))
        } else {
            Ok(())
        }
    }
}

/// One contour record in a [`PathArena`]: a command span plus cached bounds.
#[derive(Debug, Clone, Copy, Default)]
pub struct Contour {
    pub cmds: Span,
    pub bbox: BBox,
}

/// A styled path: how a contour span is painted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Path {
    pub contours: Span,
    pub bbox: BBox,
    pub paint: Paint,
}

impl Path {
    pub fn filled(rule: FillRule) -> Self {
        Self {
            contours: Span::EMPTY,
            bbox: BBox::empty(),
            paint: Paint::Fill { rule },
        }
    }

    pub fn stroked(stroke: StrokeStyle) -> Self {
        Self {
            contours: Span::EMPTY,
            bbox: BBox::empty(),
            paint: Paint::Stroke(stroke),
        }
    }

    pub fn unpainted() -> Self {
        Self {
            contours: Span::EMPTY,
            bbox: BBox::empty(),
            paint: Paint::None,
        }
    }

    pub fn is_filled(&self) -> bool {
        matches!(self.paint, Paint::Fill { .. })
    }

    pub fn is_stroked(&self) -> bool {
        matches!(self.paint, Paint::Stroke(_))
    }

    pub fn fill_rule(&self) -> Option<FillRule> {
        self.paint.fill_rule()
    }

    pub fn stroke(&self) -> Option<StrokeStyle> {
        self.paint.stroke()
    }
}

/// The flat path arena embedded in every dialect document.
#[derive(Debug, Clone, Default)]
pub struct PathArena {
    pub paths: Vec<Path>,
    pub contours: Vec<Contour>,
    pub cmds: Vec<PathCmd>,
}

impl PathArena {
    /// Append a styled path over the given contours. Returns the path index.
    ///
    /// The path bbox is the union of contour bounds, expanded by half the
    /// stroke width for stroked paints.
    pub fn push_path(
        &mut self,
        paint: Paint,
        contours: impl IntoIterator<Item = ContourBuf>,
    ) -> u32 {
        let (span, bbox) = self.push_contours(contours);
        let path_id = self.paths.len() as u32;
        self.paths.push(Path {
            contours: span,
            bbox: painted_bbox(bbox, paint),
            paint,
        });
        path_id
    }

    /// Append contours without a styled path record (profiles reference
    /// contour geometry through unpainted paths; passes append raw runs).
    pub fn push_contours(
        &mut self,
        contours: impl IntoIterator<Item = ContourBuf>,
    ) -> (Span, BBox) {
        let start = self.contours.len() as u32;
        let mut bbox = BBox::empty();
        for contour in contours {
            bbox = bbox.union(contour.bbox);
            self.push_contour(contour);
        }
        let span = Span::new(start, self.contours.len() as u32 - start);
        (span, bbox)
    }

    fn push_contour(&mut self, contour: ContourBuf) -> u32 {
        let cmd_start = self.cmds.len() as u32;
        self.cmds.extend(contour.cmds);
        let id = self.contours.len() as u32;
        self.contours.push(Contour {
            cmds: Span::new(cmd_start, self.cmds.len() as u32 - cmd_start),
            bbox: contour.bbox,
        });
        id
    }

    pub fn path(&self, path: u32) -> &Path {
        &self.paths[path as usize]
    }

    pub fn contours(&self, span: Span) -> &[Contour] {
        span.slice(&self.contours)
    }

    pub fn cmds(&self, contour: Contour) -> &[PathCmd] {
        contour.cmds.slice(&self.cmds)
    }

    /// Detach a contour span as owned contours.
    pub fn contour_bufs(&self, span: Span) -> Vec<ContourBuf> {
        self.contours(span)
            .iter()
            .map(|contour| ContourBuf::from_parts(contour.bbox, self.cmds(*contour).to_vec()))
            .collect()
    }

    /// Detach the contours of a path.
    pub fn path_contours(&self, path: &Path) -> Vec<ContourBuf> {
        self.contour_bufs(path.contours)
    }

    /// Detach a contour span, transformed.
    pub fn transformed_contour_bufs(&self, span: Span, transform: Affine2) -> Vec<ContourBuf> {
        self.contours(span)
            .iter()
            .map(|contour| transform_cmds(self.cmds(*contour).iter().copied(), transform))
            .collect()
    }

    /// Union of contour bounds over a contour span.
    pub fn contours_bbox(&self, span: Span) -> BBox {
        self.contours(span)
            .iter()
            .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox))
    }

    /// Union of contour bounds over a transformed contour span.
    pub fn transformed_contours_bbox(&self, span: Span, transform: Affine2) -> BBox {
        self.transformed_contour_bufs(span, transform)
            .iter()
            .fold(BBox::empty(), |bbox, contour| bbox.union(contour.bbox))
    }

    /// Union of path bounds over a path span.
    pub fn paths_bbox(&self, span: Span) -> BBox {
        span.slice(&self.paths)
            .iter()
            .fold(BBox::empty(), |bbox, path| bbox.union(path.bbox))
    }

    /// Copy a path (with its contours) from another arena, optionally
    /// transformed. Returns the new path index.
    pub fn append_path_from(&mut self, other: &PathArena, path: u32, transform: Affine2) -> u32 {
        let source = other.paths[path as usize];
        let contours = if transform.is_identity() {
            other.path_contours(&source)
        } else {
            other.transformed_contour_bufs(source.contours, transform)
        };
        self.push_path(source.paint, contours)
    }

    /// Recompute contour and path bounds bottom-up from the command stream.
    pub fn recompute_bounds(&mut self) {
        for contour in &mut self.contours {
            contour.bbox = contour_bbox(contour.cmds.slice(&self.cmds));
        }
        for index in 0..self.paths.len() {
            let path = self.paths[index];
            let bbox = self.contours_bbox(path.contours);
            self.paths[index].bbox = painted_bbox(bbox, path.paint);
        }
    }

    /// Drop paths not marked live, compacting contours and commands with
    /// them. Returns the old-index → new-index mapping for live paths.
    ///
    /// Passes that rewrite features leave orphaned paths behind; run this at
    /// the end of a pass pipeline and remap stored path spans with the
    /// returned table.
    pub fn compact(&mut self, live: &[bool]) -> Vec<Option<u32>> {
        assert_eq!(live.len(), self.paths.len());
        let mut mapping = vec![None; self.paths.len()];
        let mut paths = Vec::new();
        let mut contours = Vec::new();
        let mut cmds = Vec::new();

        for (index, path) in self.paths.iter().enumerate() {
            if !live[index] {
                continue;
            }
            let contour_start = contours.len() as u32;
            for contour in self.contours(path.contours) {
                let cmd_start = cmds.len() as u32;
                cmds.extend_from_slice(self.cmds(*contour));
                contours.push(Contour {
                    cmds: Span::new(cmd_start, cmds.len() as u32 - cmd_start),
                    bbox: contour.bbox,
                });
            }
            mapping[index] = Some(paths.len() as u32);
            paths.push(Path {
                contours: Span::new(contour_start, contours.len() as u32 - contour_start),
                ..*path
            });
        }

        self.paths = paths;
        self.contours = contours;
        self.cmds = cmds;
        mapping
    }

    pub fn validate(&self, name: &str) -> Result<(), crate::geom::Diagnostics> {
        let mut diagnostics = crate::geom::Diagnostics::default();
        self.validate_into(name, &mut diagnostics);
        diagnostics.into_result()
    }

    /// Collect validation problems without terminating at the first.
    pub fn validate_into(&self, name: &str, diagnostics: &mut crate::geom::Diagnostics) {
        for (index, path) in self.paths.iter().enumerate() {
            if let Err(message) =
                path.contours
                    .validate(&format!("{name} path contours"), index, self.contours.len())
            {
                diagnostics.error(message);
            }
            if let Err(message) = validate_bbox(&format!("{name} path"), index, path.bbox) {
                diagnostics.error(message);
            }
        }
        for (index, contour) in self.contours.iter().enumerate() {
            if let Err(message) =
                contour
                    .cmds
                    .validate(&format!("{name} contour commands"), index, self.cmds.len())
            {
                diagnostics.error(message);
            }
            if let Err(message) = validate_bbox(&format!("{name} contour"), index, contour.bbox) {
                diagnostics.error(message);
            }
        }
        if let Err(message) = validate_cmd_points(name, &self.cmds) {
            diagnostics.error(message);
        }
    }
}

fn painted_bbox(bbox: BBox, paint: Paint) -> BBox {
    match paint.stroke() {
        Some(stroke) => bbox.expand(stroke.width / 2.0),
        None => bbox,
    }
}

pub(crate) fn validate_bbox(name: &str, index: usize, bbox: BBox) -> Result<(), String> {
    if bbox.is_valid() {
        Ok(())
    } else {
        Err(format!("{name} {index} has invalid bbox {bbox:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::point::Point;
    use crate::geom::style::LineCap;

    #[test]
    fn push_path_records_contours_and_bounds() {
        let mut arena = PathArena::default();

        let path = arena.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 2.0, 1.0)],
        );

        let path = arena.path(path);
        assert_eq!(path.contours.len(), 1);
        assert_eq!(path.bbox.min, Point::new(0.0, 0.0));
        assert_eq!(path.bbox.max, Point::new(2.0, 1.0));
        arena.validate("test").unwrap();
    }

    #[test]
    fn stroked_path_bbox_expands_by_half_width() {
        let mut arena = PathArena::default();

        let path = arena.push_path(
            Paint::Stroke(StrokeStyle::new(1.0, LineCap::Round)),
            [ContourBuf::new(vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(4.0, 0.0)),
            ])],
        );

        assert_eq!(arena.path(path).bbox.min, Point::new(-0.5, -0.5));
        assert_eq!(arena.path(path).bbox.max, Point::new(4.5, 0.5));
    }

    #[test]
    fn append_path_from_copies_across_arenas_with_transform() {
        let mut source = PathArena::default();
        let path = source.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0)],
        );

        let mut target = PathArena::default();
        let copied =
            target.append_path_from(&source, path, Affine2::translation(Point::new(10.0, 0.0)));

        assert_eq!(target.path(copied).bbox.min, Point::new(10.0, 0.0));
        assert_eq!(target.path(copied).bbox.max, Point::new(11.0, 1.0));
        target.validate("target").unwrap();
    }

    #[test]
    fn compact_drops_dead_paths_and_remaps() {
        let mut arena = PathArena::default();
        let a = arena.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(0.0, 0.0, 1.0, 1.0)],
        );
        let b = arena.push_path(
            Paint::Fill {
                rule: FillRule::NonZero,
            },
            [rect_contour(5.0, 5.0, 6.0, 6.0)],
        );

        let mut live = vec![false; arena.paths.len()];
        live[b as usize] = true;
        let mapping = arena.compact(&live);

        assert_eq!(mapping[a as usize], None);
        assert_eq!(mapping[b as usize], Some(0));
        assert_eq!(arena.paths.len(), 1);
        assert_eq!(arena.paths[0].bbox.min, Point::new(5.0, 5.0));
        arena.validate("test").unwrap();
    }

    fn rect_contour(x0: f64, y0: f64, x1: f64, y1: f64) -> ContourBuf {
        ContourBuf::new(vec![
            PathCmd::move_to(Point::new(x0, y0)),
            PathCmd::line_to(Point::new(x1, y0)),
            PathCmd::line_to(Point::new(x1, y1)),
            PathCmd::line_to(Point::new(x0, y1)),
            PathCmd::close(),
        ])
    }
}
