//! Polygon regularization, fixed-grid simplification, and inward decimation.

use super::{ContourSet, Ring, Shape, edges_of, flatten_shapes, overlay_fill_rule, rings_bbox};
use crate::geom::accuracy::{lattice_per_mm, numerical_error};
use crate::geom::dist;
use crate::geom::{AccuracyError, BBox, FillRule, Point, tol};
use i_overlay::core::extract::BooleanExtractionBuffer;
use i_overlay::core::fill_rule::FillRule as OverlayFillRule;
use i_overlay::core::overlay::{IntOverlayOptions, Overlay};
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::simplify::Simplify;
use i_overlay::i_float::int::point::IntPoint;
use i_overlay::i_shape::flat::buffer::FlatContoursBuffer;
use i_overlay::i_shape::int::shape::IntShapes;
use std::collections::VecDeque;
use std::hash::Hasher;
use std::sync::{Arc, Mutex};

struct Regularization {
    fingerprint: u64,
    fill: OverlayFillRule,
    input: Vec<Ring>,
    output: Arc<Vec<Shape>>,
    vertices: usize,
}

static REGULARIZATIONS: Mutex<VecDeque<Regularization>> = Mutex::new(VecDeque::new());
const CACHED_VERTICES: usize = 4_000_000;

/// Regularize rings under the given fill rule into non-overlapping shapes.
pub(crate) fn simplify_rings(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Ring> {
    flatten_shapes(simplify_shapes(rings, fill_rule))
}

/// Regularize rings keeping the connected-shape structure: each shape is its
/// outer ring followed by its holes, wound opposite.
///
/// Rings whose bounds never touch cannot nest or cross under any fill rule,
/// so each group of rings connected through their bounds is regularized on
/// its own. The overlay's split and sort stages then scale with the group,
/// not the layer, which matters for silkscreen and mask images made of
/// thousands of small, locally overlapping features.
pub fn simplify_shapes(rings: Vec<Ring>, fill_rule: FillRule) -> Vec<Shape> {
    regularize(rings, overlay_fill_rule(fill_rule))
}

fn regularize(rings: Vec<Ring>, rule: OverlayFillRule) -> Vec<Shape> {
    bounds_connected_groups(rings.into_iter().map(|ring| (ring, ())).collect(), 0.0)
        .into_iter()
        .flat_map(|group| overlay(&untagged(group), &[], OverlayRule::Subject, rule))
        .collect()
}

pub(super) fn overlay(
    subject: &[Ring],
    clip: &[Ring],
    rule: OverlayRule,
    fill_rule: OverlayFillRule,
) -> Vec<Shape> {
    // This caches only the pure polygon operation, never a ContourSet or its
    // budget. Callers still inherit and validate their own accuracy history.
    let vertices = subject.iter().map(Vec::len).sum::<usize>();
    let fingerprint = (rule == OverlayRule::Subject
        && clip.is_empty()
        && (4096..=CACHED_VERTICES / 2).contains(&vertices))
    .then(|| {
        let mut hash = rustc_hash::FxHasher::default();
        hash.write_usize(subject.len());
        for ring in subject {
            hash.write_usize(ring.len());
            for point in ring {
                hash.write_u64(point[0].to_bits());
                hash.write_u64(point[1].to_bits());
            }
        }
        hash.finish()
    });
    if let Some(fingerprint) = fingerprint {
        let hit = REGULARIZATIONS.try_lock().ok().and_then(|mut cache| {
            let index = cache.iter().position(|entry| {
                entry.fingerprint == fingerprint
                    && entry.fill == fill_rule
                    && entry.input.len() == subject.len()
                    && entry.input.iter().zip(subject).all(|(left, right)| {
                        left.len() == right.len()
                            && left
                                .iter()
                                .flatten()
                                .zip(right.iter().flatten())
                                .all(|(a, b)| a.to_bits() == b.to_bits())
                    })
            })?;
            let entry = cache.remove(index)?;
            let output = Arc::clone(&entry.output);
            cache.push_back(entry);
            Some(output)
        });
        if let Some(output) = hit {
            return (*output).clone();
        }
    }
    let [shapes] = overlay_on_lattice(subject, clip, true, fill_rule, |subject, clip| {
        if rule == OverlayRule::Subject && clip.points.is_empty() {
            [regularize_lattice(subject, fill_rule)]
        } else {
            [Overlay::from_subj_and_clip(subject, clip).overlay(rule, fill_rule)]
        }
    });
    let retained = vertices + shapes.iter().flatten().map(Vec::len).sum::<usize>();
    if let Some(fingerprint) = fingerprint
        && retained <= CACHED_VERTICES
    {
        let entry = Regularization {
            fingerprint,
            fill: fill_rule,
            input: subject.to_vec(),
            output: Arc::new(shapes.clone()),
            vertices: retained,
        };
        if let Ok(mut cache) = REGULARIZATIONS.try_lock() {
            let mut size = cache.iter().map(|entry| entry.vertices).sum::<usize>();
            while size + retained > CACHED_VERTICES || cache.len() >= 64 {
                size -= cache.pop_front().unwrap().vertices;
            }
            cache.push_back(entry);
        }
    }
    shapes
}

/// Let the kernel detect simple contours and resolve intersections. Keep hull
/// starts canonical even when its single-contour path preserves input order.
fn regularize_lattice(subject: &FlatContoursBuffer<i64>, fill: OverlayFillRule) -> IntShapes<i64> {
    let mut shapes = subject.simplify(fill, IntOverlayOptions::default());
    for shape in &mut shapes {
        let ring = &mut shape[0];
        let first = ring
            .iter()
            .enumerate()
            .min_by_key(|(_, point)| (point.x, point.y))
            .unwrap()
            .0;
        let start = (first + ring.len() - 1) % ring.len();
        ring.rotate_left(start);
    }
    shapes
}

/// Convex orientation requiring nonincident vertices to clear
/// every supporting line by `margin` lattice units. On a convex contour the
/// nearest such vertices are the two adjacent to the edge's endpoints.
fn convex_turn(points: &[IntPoint<i64>], margin: f64) -> Option<i128> {
    if points.len() < 3 {
        return None;
    }
    let mut turn = 0;
    let mut first_direction = 0;
    let mut direction = 0;
    let mut reversals = 0;
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];
        let c = points[(index + 2) % points.len()];
        let cross = (b.x as i128 - a.x as i128) * (c.y as i128 - b.y as i128)
            - (b.y as i128 - a.y as i128) * (c.x as i128 - b.x as i128);
        let sign = cross.signum();
        if sign == 0 || (turn != 0 && sign != turn) {
            return None;
        }
        // L1 length bounds Euclidean length from above, so this comparison
        // is a conservative lower bound on distance to either adjacent line.
        let ab = (b.x as i128 - a.x as i128).abs() + (b.y as i128 - a.y as i128).abs();
        let bc = (c.x as i128 - b.x as i128).abs() + (c.y as i128 - b.y as i128).abs();
        if cross.abs() as f64 <= margin * ab.max(bc) as f64 {
            return None;
        }
        turn = sign;
        let next = b.x.cmp(&a.x) as i8;
        if next != 0 {
            if first_direction == 0 {
                first_direction = next;
            } else if next != direction {
                reversals += 1;
            }
            direction = next;
        }
    }
    reversals += usize::from(first_direction != direction);
    (reversals == 2).then_some(turn)
}

/// Several rules over one overlay of the same operands. The graph is split,
/// filled and linked once; each rule's extraction skips the links it
/// excludes, so it yields exactly the shapes [`overlay`] yields for it.
pub(super) fn overlay_rules<const N: usize>(
    subject: &[Ring],
    clip: &[Ring],
    rules: [OverlayRule; N],
    fill_rule: OverlayFillRule,
) -> [Vec<Shape>; N] {
    overlay_on_lattice(subject, clip, true, fill_rule, |subject, clip| {
        let mut overlay = Overlay::from_subj_and_clip(subject, clip);
        let mut buffer = BooleanExtractionBuffer::default();
        match overlay.build_graph_view(fill_rule) {
            Some(graph) => rules.map(|rule| graph.extract_shapes(rule, &mut buffer)),
            None => rules.map(|_| Vec::new()),
        }
    })
}

/// The region a constructed curve winds positively around, on the lattice
/// booleans use. Its edges are not split at vertices lying on them: that
/// cancels seams where source fills were cut along each other, and a curve
/// built from regularized rings has none.
pub(super) fn positive_winding(rings: &[Ring]) -> Vec<Ring> {
    let [shapes] = overlay_on_lattice(
        rings,
        &[],
        false,
        OverlayFillRule::Positive,
        |subject, _| [regularize_lattice(subject, OverlayFillRule::Positive)],
    );
    flatten_shapes(shapes)
}

/// Overlay operands on their shared lattice. With `split`, every edge is
/// first split at the vertices lying on it.
fn overlay_on_lattice<const N: usize>(
    subject: &[Ring],
    clip: &[Ring],
    split: bool,
    fill_rule: OverlayFillRule,
    extract: impl FnOnce(&FlatContoursBuffer<i64>, &FlatContoursBuffer<i64>) -> [IntShapes<i64>; N],
) -> [Vec<Shape>; N] {
    let bbox = rings_bbox(subject).union(rings_bbox(clip));
    let scale = lattice_per_mm(bbox);
    let isolated = (split && subject.len() == 1 && clip.is_empty())
        .then(|| {
            let points = subject[0]
                .iter()
                .map(|&[x, y]| {
                    IntPoint::new((x * scale).round() as i64, (y * scale).round() as i64)
                })
                .collect::<Vec<_>>();
            // Keep the joint lattice. Only omit the vertex index when no
            // nonincident vertex could pass its on-edge test, even after
            // source arithmetic and integer rounding move the supporting line.
            let margin = (ON_EDGE_MM + 2.0 * numerical_error(bbox)) * scale + 4.0;
            convex_turn(&points, margin)?;
            let ranges = std::iter::once(0..points.len()).collect();
            Some(FlatContoursBuffer { points, ranges })
        })
        .flatten();
    let (subject, clip) = if let Some(subject) = isolated {
        (subject, FlatContoursBuffer::default())
    } else {
        let vertices = split.then(|| VertexColumns::new(&[subject, clip], bbox));
        let mut on_edge = Vec::new();
        // Point arenas and contour ranges replace one allocation per ring.
        // Preserve the split points and their order before optional exact
        // rectangle consolidation; reserve kernel segments from this count.
        let mut lattice = |rings: &[Ring]| {
            let mut buffer = FlatContoursBuffer {
                points: Vec::with_capacity(rings.iter().map(Vec::len).sum()),
                ranges: Vec::with_capacity(rings.len()),
            };
            for ring in rings {
                let start = buffer.points.len();
                let mut emit = |[x, y]: [f64; 2]| {
                    buffer.points.push(IntPoint::new(
                        (x * scale).round() as i64,
                        (y * scale).round() as i64,
                    ));
                };
                match &vertices {
                    Some(vertices) => vertices.split(ring, &mut on_edge, &mut emit),
                    None => ring.iter().copied().for_each(&mut emit),
                }
                buffer.ranges.push(start..buffer.points.len());
            }
            buffer
        };
        // Scratch arenas and the vertex index die before graph construction.
        let mut prepared_subject = lattice(subject);
        let clip = lattice(clip);
        // A large union of positive rectangles can lose internal strip edges
        // exactly on the existing lattice. Do not alter winding multiplicities
        // under parity fill or in the presence of negative/nonrectangular rings.
        if clip.points.is_empty()
            && fill_rule == OverlayFillRule::NonZero
            && subject.len() > 256
            && subject.iter().all(|ring| match ring.as_slice() {
                [a, b, c, d] => {
                    a[1] == b[1]
                        && b[0] == c[0]
                        && c[1] == d[1]
                        && d[0] == a[0]
                        && a[0] < b[0]
                        && a[1] < d[1]
                }
                _ => false,
            })
        {
            coalesce_rectangle_strips(&mut prepared_subject, (ON_EDGE_MM * scale).ceil() as i64);
        }
        (prepared_subject, clip)
    };
    extract(&subject, &clip).map(|shapes| {
        shapes
            .into_iter()
            .map(|shape| {
                shape
                    .into_iter()
                    .map(|ring| {
                        ring.into_iter()
                            .map(|point| [point.x as f64 / scale, point.y as f64 / scale])
                            .collect()
                    })
                    .collect()
            })
            .collect()
    })
}

/// Consolidate equal-span, touching or overlapping rectangles after all source
/// edge splitting and lattice conversion. Kinked contours remain untouched.
/// Two in-place sort/compact passes bound construction to O(n log n) and O(n)
/// scratch, rather than iterating a potentially quadratic strip fixed point.
fn coalesce_rectangle_strips(buffer: &mut FlatContoursBuffer<i64>, guard: i64) {
    let mut candidates = Vec::with_capacity(buffer.ranges.len());
    let mut others = Vec::new();
    for range in &buffer.ranges {
        let ring = &buffer.points[range.clone()];
        let min_x = ring.iter().map(|p| p.x).min().unwrap();
        let max_x = ring.iter().map(|p| p.x).max().unwrap();
        let min_y = ring.iter().map(|p| p.y).min().unwrap();
        let max_y = ring.iter().map(|p| p.y).max().unwrap();
        let mut length = 0_u128;
        let mut right_rise = 0_i128;
        let on_frame = ring.iter().zip(ring.iter().cycle().skip(1)).all(|(a, b)| {
            length += (b.x as i128 - a.x as i128).unsigned_abs()
                + (b.y as i128 - a.y as i128).unsigned_abs();
            if a.x == max_x && b.x == max_x {
                right_rise += b.y as i128 - a.y as i128;
            }
            (a.x == b.x && (a.x == min_x || a.x == max_x))
                || (a.y == b.y && (a.y == min_y || a.y == max_y))
        });
        let width = max_x as i128 - min_x as i128;
        let height = max_y as i128 - min_y as i128;
        if on_frame
            && width > 0
            && height > 0
            && right_rise == height
            && length == 2 * (width + height) as u128
        {
            candidates.push((range.clone(), [min_x, max_x, min_y, max_y]));
        } else {
            others.push((range.clone(), [min_x, max_x, min_y, max_y]));
        }
    }
    // Splitting can kink a rectangle, even creating a negative-winding
    // pocket. Preserve multiplicities wherever such a contour can interact.
    // Bound the exception scan; general batches keep the ordinary overlay.
    if others.len() > 64 {
        return;
    }
    let exceptions = others.iter().map(|(_, bounds)| *bounds).collect::<Vec<_>>();
    let mut rectangles = Vec::with_capacity(candidates.len());
    for (range, bounds) in candidates {
        let protected = exceptions.iter().any(|other| {
            bounds[0] as i128 <= other[1] as i128 + guard as i128
                && bounds[1] as i128 >= other[0] as i128 - guard as i128
                && bounds[2] as i128 <= other[3] as i128 + guard as i128
                && bounds[3] as i128 >= other[2] as i128 - guard as i128
        });
        if protected {
            others.push((range, bounds));
        } else {
            rectangles.push(bounds);
        }
    }
    let before = rectangles.len();
    for _ in 0..2 {
        rectangles.sort_unstable();
        let mut kept = 0;
        for index in 0..rectangles.len() {
            let rectangle = rectangles[index];
            if kept > 0
                && rectangles[kept - 1][0..2] == rectangle[0..2]
                && rectangle[2] <= rectangles[kept - 1][3]
            {
                rectangles[kept - 1][3] = rectangles[kept - 1][3].max(rectangle[3]);
            } else {
                rectangles[kept] = rectangle;
                kept += 1;
            }
        }
        rectangles.truncate(kept);
        for rectangle in &mut rectangles {
            let [a, b, c, d] = *rectangle;
            *rectangle = [c, d, a, b];
        }
    }
    if let Some(union) = rectangle_union_strips(&rectangles) {
        rectangles = union;
    }
    if rectangles.len() == before {
        return;
    }
    let mut merged = FlatContoursBuffer {
        points: Vec::with_capacity(
            others.iter().map(|(range, _)| range.len()).sum::<usize>() + 4 * rectangles.len(),
        ),
        ranges: Vec::with_capacity(others.len() + rectangles.len()),
    };
    for (range, _) in others {
        let start = merged.points.len();
        merged.points.extend_from_slice(&buffer.points[range]);
        merged.ranges.push(start..merged.points.len());
    }
    for [min_x, max_x, min_y, max_y] in rectangles {
        let start = merged.points.len();
        merged.points.extend([
            IntPoint::new(min_x, min_y),
            IntPoint::new(max_x, min_y),
            IntPoint::new(max_x, max_y),
            IntPoint::new(min_x, max_y),
        ]);
        merged.ranges.push(start..merged.points.len());
    }
    *buffer = merged;
}

/// Exact sweep over the already-snapped positive rectangles. Coverage counts
/// retain overlap multiplicity; only covered spans are emitted. A bounded
/// traversal falls back to the original rectangles for fragmented sweeps.
fn rectangle_union_strips(rectangles: &[[i64; 4]]) -> Option<Vec<[i64; 4]>> {
    use std::collections::BTreeMap;
    if rectangles.len() < 2 {
        return None;
    }
    let mut ys = rectangles
        .iter()
        .flat_map(|r| [r[2], r[3]])
        .collect::<Vec<_>>();
    ys.sort_unstable();
    ys.dedup();
    let mut events = Vec::with_capacity(2 * rectangles.len());
    for &[left, right, bottom, top] in rectangles {
        let low = ys.binary_search(&bottom).unwrap();
        let high = ys.binary_search(&top).unwrap();
        events.extend([(left, low, high, 1_i32), (right, low, high, -1)]);
    }
    events.sort_unstable();
    struct Coverage {
        // The range's own count, and the minimum/maximum including children.
        nodes: Vec<[i32; 3]>,
    }
    impl Coverage {
        fn add(&mut self, node: usize, span: (usize, usize), range: (usize, usize), delta: i32) {
            let (left, right) = span;
            if range.0 <= left && right <= range.1 {
                for count in &mut self.nodes[node] {
                    *count += delta;
                }
                return;
            }
            let middle = (left + right) / 2;
            if range.0 < middle {
                self.add(2 * node, (left, middle), range, delta);
            }
            if range.1 > middle {
                self.add(2 * node + 1, (middle, right), range, delta);
            }
            self.nodes[node][1] =
                self.nodes[node][0] + self.nodes[2 * node][1].min(self.nodes[2 * node + 1][1]);
            self.nodes[node][2] =
                self.nodes[node][0] + self.nodes[2 * node][2].max(self.nodes[2 * node + 1][2]);
        }

        fn spans(
            &self,
            node: usize,
            ys: &[i64],
            inherited: i32,
            work: &mut usize,
            out: &mut Vec<(i64, i64)>,
        ) -> Option<()> {
            *work = work.checked_sub(1)?;
            let [local, low, high] = self.nodes[node];
            if low + inherited > 0 {
                let (start, end) = (ys[0], ys[ys.len() - 1]);
                if let Some(last) = out.last_mut()
                    && last.1 == start
                {
                    last.1 = end;
                } else {
                    out.push((start, end));
                }
            } else if high + inherited > 0 {
                let middle = (ys.len() - 1) / 2;
                self.spans(2 * node, &ys[..=middle], inherited + local, work, out)?;
                self.spans(2 * node + 1, &ys[middle..], inherited + local, work, out)?;
            }
            Some(())
        }
    }
    let mut coverage = Coverage {
        nodes: vec![[0; 3]; 4 * ys.len()],
    };
    let mut work = 128 * rectangles.len();
    let mut result: Vec<[i64; 4]> = Vec::new();
    let mut active = BTreeMap::<(i64, i64), usize>::new();
    let mut index = 0;
    let mut spans = Vec::new();
    while index < events.len() {
        let x = events[index].0;
        while index < events.len() && events[index].0 == x {
            let (_, low, high, delta) = events[index];
            coverage.add(1, (0, ys.len() - 1), (low, high), delta);
            index += 1;
        }
        let Some(&(next, ..)) = events.get(index) else {
            break;
        };
        spans.clear();
        coverage.spans(1, &ys, 0, &mut work, &mut spans)?;
        let mut continued = BTreeMap::new();
        for &span in &spans {
            let id = match active.remove(&span) {
                Some(id) => {
                    result[id][1] = next;
                    id
                }
                None => {
                    result.push([x, next, span.0, span.1]);
                    result.len() - 1
                }
            };
            continued.insert(span, id);
        }
        if result.len() >= rectangles.len() {
            return None;
        }
        active = continued;
    }
    Some(result)
}

/// The vertices of one overlay, indexed to find those lying on an edge.
///
/// A source tool cuts one fill along another's edge and rounds the ends of
/// the cut to its grid, so the fills' shared stretch is two edges a fraction
/// of a grid step apart and an exact overlay keeps the hairline between
/// them. Splitting every edge at the vertices that lie on it makes the
/// stretch one edge again, which the overlay cancels.
struct VertexColumns {
    /// Vertices by column, ascending in `y` within each.
    points: Vec<[f64; 2]>,
    /// Offsets into `points` per column; two longer than the column count.
    columns: Vec<u32>,
    origin: f64,
    pitch: f64,
}

impl VertexColumns {
    fn new(rings: &[&[Ring]], bbox: BBox) -> Self {
        let rings = || rings.iter().flat_map(|rings| rings.iter());
        let count = rings().map(Vec::len).sum::<usize>();
        // Columns about one edge wide: an edge then reads a column or two
        // and finds a vertex or two there, however the edges are spread.
        let extent = rings()
            .flat_map(|ring| edges_of(ring))
            .map(|(start, end)| (end.x - start.x).abs() + (end.y - start.y).abs())
            .sum::<f64>()
            .max(bbox.width());
        let (origin, pitch) = (bbox.min.x, (extent / count as f64).max(ON_EDGE_MM));
        let column = |x: f64| ((x - origin) / pitch) as usize;
        // Counted two slots ahead, a column's offset is first where its
        // vertices go next and then, once they are placed, where it starts.
        let mut columns = vec![0u32; column(bbox.max.x) + 3];
        for [x, _] in rings().flatten() {
            columns[column(*x) + 2] += 1;
        }
        for column in 1..columns.len() {
            columns[column] += columns[column - 1];
        }
        let mut points = vec![[0.0; 2]; count];
        for point in rings().flatten() {
            let slot = &mut columns[column(point[0]) + 1];
            points[*slot as usize] = *point;
            *slot += 1;
        }
        // Coincident vertices are one vertex, so stacked copies of a shape
        // cost no more to search than one.
        let mut kept = 0;
        for column in 0..columns.len() - 1 {
            let (start, end) = (columns[column] as usize, columns[column + 1] as usize);
            points[start..end].sort_unstable_by(|left, right| {
                left[1]
                    .total_cmp(&right[1])
                    .then(left[0].total_cmp(&right[0]))
            });
            columns[column] = kept as u32;
            for index in start..end {
                if kept == columns[column] as usize || points[index] != points[kept - 1] {
                    points[kept] = points[index];
                    kept += 1;
                }
            }
        }
        *columns.last_mut().unwrap() = kept as u32;
        points.truncate(kept);
        Self {
            points,
            columns,
            origin,
            pitch,
        }
    }

    fn column(&self, x: f64) -> usize {
        ((x - self.origin) / self.pitch) as usize
    }

    /// Emit `ring` with every edge split at the vertices lying on it.
    fn split(
        &self,
        ring: &[[f64; 2]],
        on_edge: &mut Vec<(f64, [f64; 2])>,
        mut emit: impl FnMut([f64; 2]),
    ) {
        let reach = ON_EDGE_MM;
        let last = self.columns.len() - 3;
        for (index, &[ax, ay]) in ring.iter().enumerate() {
            let [bx, by] = ring[(index + 1) % ring.len()];
            let (dx, dy) = (bx - ax, by - ay);
            let length2 = dx * dx + dy * dy;
            let (min_x, max_x) = (ax.min(bx) - reach, ax.max(bx) + reach);
            let (min_y, max_y) = (ay.min(by) - reach, ay.max(by) + reach);
            for column in self.column(min_x)..=self.column(max_x).min(last) {
                // The edge's span of `y` within this column alone, so a long
                // diagonal reads the vertices along it, not its bounding box.
                let (low, high) = if dx == 0.0 {
                    (min_y, max_y)
                } else {
                    let left = self.origin + column as f64 * self.pitch;
                    let y_at = |x: f64| ay + (x.clamp(min_x, max_x) - ax) * dy / dx;
                    let (y0, y1) = (y_at(left - reach), y_at(left + self.pitch + reach));
                    (
                        (y0.min(y1) - reach).clamp(min_y, max_y),
                        (y0.max(y1) + reach).clamp(min_y, max_y),
                    )
                };
                let points =
                    &self.points[self.columns[column] as usize..self.columns[column + 1] as usize];
                let first = points.partition_point(|point| point[1] < low);
                for &[x, y] in points[first..].iter().take_while(|point| point[1] <= high) {
                    // Strictly between the ends, which the edge already has.
                    let along = (x - ax) * dx + (y - ay) * dy;
                    let across = (x - ax) * dy - (y - ay) * dx;
                    if along > 0.0 && along < length2 && across * across <= reach * reach * length2
                    {
                        on_edge.push((along, [x, y]));
                    }
                }
            }
            on_edge.sort_by(|left, right| left.0.total_cmp(&right.0));
            emit([ax, ay]);
            on_edge.drain(..).for_each(|(_, point)| emit(point));
        }
    }
}

/// How far a vertex may sit from an edge it lies on: source tools round
/// the vertex and the edge's ends to their grid, each by up to half a cell
/// diagonal.
pub(super) const ON_EDGE_MM: f64 = std::f64::consts::SQRT_2 * tol::SOURCE_GRID_MM;

/// Resolve tagged rings one bounds-connected group at a time.
///
/// Rings whose bounds never touch cannot interact under any set operation,
/// so `resolve` sees only rings that can, each tagged with the operand it
/// came from, and decides what that group contributes. An operation over a
/// panel then costs what its local neighbourhoods cost, however the caller
/// happened to batch it.
pub(super) fn resolve_groups<T>(
    rings: Vec<(Ring, T)>,
    reach: f64,
    resolve: impl FnMut(Vec<(Ring, T)>) -> Vec<Ring>,
) -> Vec<Ring> {
    bounds_connected_groups(rings, reach)
        .into_iter()
        .flat_map(resolve)
        .collect()
}

pub(super) fn untagged<T>(group: Vec<(Ring, T)>) -> Vec<Ring> {
    group.into_iter().map(|(ring, _)| ring).collect()
}

/// Partition rings into groups connected by overlapping bounds, each in
/// input order and the groups ordered by their first ring, so callers that
/// number the resulting shapes see the same numbering for the same input.
///
/// Bounds carry the overlay's rounding allowance and its reach from an edge
/// to a vertex on it: every group snaps to its own integer grid, and groups
/// that stay apart by more than that cannot be joined. A sweep in `x` keeps
/// one hull per open group and merges a ring into every group whose hull it
/// meets. The hull stands in for its members, so it may merge groups no
/// member pair joins; that costs only partitioning benefit, never
/// correctness, and lets a layer of long features degenerate to the single
/// overlay it needed before.
///
/// `reach` widens every ring's bounds further, for operations that join
/// rings that far apart.
pub(super) fn bounds_connected_groups<T>(rings: Vec<(Ring, T)>, reach: f64) -> Vec<Vec<(Ring, T)>> {
    if rings.len() == 1 {
        return vec![rings];
    }
    struct Group {
        hull: BBox,
        root: usize,
    }
    let bounds = rings
        .iter()
        .map(|(ring, _)| rings_bbox(std::slice::from_ref(ring)))
        .collect::<Vec<_>>();
    let slack = numerical_error(bounds.iter().copied().fold(BBox::empty(), BBox::union))
        + ON_EDGE_MM
        + reach;
    let mut order = bounds
        .into_iter()
        .map(|bbox| bbox.expand(slack))
        .enumerate()
        .collect::<Vec<_>>();
    order.sort_by(|(_, a), (_, b)| a.min.x.total_cmp(&b.min.x));
    // Membership is a forest over ring indices: a group absorbed by a later
    // ring hangs its root under that ring, so a chain of touching rings
    // merges in constant time per link instead of recopying its members.
    let mut parent = (0..rings.len()).collect::<Vec<_>>();
    let mut open: Vec<Group> = Vec::new();
    for (index, bbox) in order {
        let mut hull = bbox;
        let mut i = 0;
        while i < open.len() {
            // A sweep line crossing a band of many separate features would
            // compare every ring against all of them; the surplus folds into
            // this group instead, bounding the work per ring.
            if open[i].hull.max.x < bbox.min.x {
                open.swap_remove(i);
            } else if open[i].hull.intersects(bbox) || open.len() > MAX_OPEN_GROUPS {
                let group = open.swap_remove(i);
                hull = hull.union(group.hull);
                parent[group.root] = index;
            } else {
                i += 1;
            }
        }
        open.push(Group { hull, root: index });
    }
    let mut group_of_root = vec![usize::MAX; rings.len()];
    let mut groups: Vec<Vec<(Ring, T)>> = Vec::new();
    for (index, ring) in rings.into_iter().enumerate() {
        let mut root = index;
        while parent[root] != root {
            parent[root] = parent[parent[root]];
            root = parent[root];
        }
        if group_of_root[root] == usize::MAX {
            group_of_root[root] = groups.len();
            groups.push(Vec::new());
        }
        groups[group_of_root[root]].push(ring);
    }
    groups
}

/// Open groups a sweep line compares each ring against before folding them.
const MAX_OPEN_GROUPS: usize = 256;

const GRID_SUBDIVISIONS: i128 = 1024;

/// Integer coordinate on a fixed grid, using translation-invariant half-up
/// rounding. Ties are decided on a 1/1024 sub-grid to absorb floating-point
/// noise. Callers must supply a positive finite grid and coordinates within
/// the exact integer range (2^50 grid steps).
pub fn grid_coordinate(value: f64, grid: f64) -> i64 {
    // The sub-grid intermediate needs more range than the output coordinate.
    ((value / grid * GRID_SUBDIVISIONS as f64 + 0.5).floor() as i128 + GRID_SUBDIVISIONS / 2)
        .div_euclid(GRID_SUBDIVISIONS) as i64
}

/// Maximum planar displacement from rounding both axes with [`grid_coordinate`],
/// including its half-sub-grid tie tolerance.
pub fn grid_rounding_error(grid: f64) -> f64 {
    grid * (1.0 + 1.0 / GRID_SUBDIVISIONS as f64) / std::f64::consts::SQRT_2
}

/// Regularize filled rings on an exact output grid.
///
/// The fixed-scale integer overlay resolves crossings and removes coincident
/// vertices while snapping every result vertex to `grid`. Geometry that
/// collapses during coordinate quantization is not representable on that
/// output grid.
pub(super) fn integer_shapes_on_grid(
    rings: Vec<Ring>,
    fill_rule: FillRule,
    grid: f64,
    options: IntOverlayOptions<u128>,
) -> IntShapes<i64> {
    let rings = rings
        .into_iter()
        .map(|ring| {
            ring.into_iter()
                .map(|[x, y]| IntPoint::new(grid_coordinate(x, grid), grid_coordinate(y, grid)))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    rings
        .as_slice()
        .simplify(overlay_fill_rule(fill_rule), options)
}

/// Decimate rings so the region only shrinks: the result covers no point the
/// source did not, and no source vertex ends farther than `deviation_mm`
/// from the decimated boundary.
pub(crate) fn decimate_rings_inward<'a>(
    rings: impl Iterator<Item = &'a [[f64; 2]]>,
    deviation_mm: f64,
) -> Vec<Ring> {
    rings
        .map(|ring| decimate_ring_inward(ring, deviation_mm))
        .collect()
}

fn decimate_ring_inward(ring: &[[f64; 2]], deviation_mm: f64) -> Ring {
    if ring.len() < 4 {
        return ring.to_vec();
    }
    let point = |index: usize| {
        let [x, y] = ring[index % ring.len()];
        Point::new(x, y)
    };
    // Rings keep material on the left of travel, so a chord absorbs the
    // vertices between its ends exactly when every one lies on the chord's
    // right — the removed bulge is material — and within the deviation.
    // Every chord re-checks its whole chain, so error cannot accumulate.
    let chord_absorbs = |anchor: usize, end: usize| {
        let start = point(anchor);
        let endpoint = point(end);
        let chord = endpoint - start;
        // A chord back to its own anchor has no side for a vertex to be on.
        if start == endpoint {
            return false;
        }
        (anchor + 1..end).all(|index| {
            let offset = point(index) - start;
            let cross = chord.x * offset.y - chord.y * offset.x;
            cross <= 0.0 && dist::point_segment(point(index), start, endpoint).0 <= deviation_mm
        })
    };

    let mut kept = vec![ring[0]];
    let mut anchor = 0;
    while anchor + 1 < ring.len() {
        // Grow the chord greedily; `end == ring.len()` is the closing chord
        // back to the first vertex, which absorbs the remaining tail.
        let mut end = anchor + 1;
        while end < ring.len() && chord_absorbs(anchor, end + 1) {
            end += 1;
        }
        if end == ring.len() {
            break;
        }
        kept.push(ring[end]);
        anchor = end;
    }
    if kept.len() < 3 {
        return ring.to_vec();
    }
    kept
}

impl ContourSet {
    /// Decimate the region's boundary so it only shrinks; see
    /// `decimate_rings_inward`.
    ///
    /// A chord takes one turn of winding off what it cuts from its ring and
    /// changes nothing elsewhere, so the decimated rings wind no point more
    /// than the source did and positive winding keeps a subset of it. The
    /// nonzero rule would not: a hole left outside its ring by a chord winds
    /// negatively there and would fill.
    pub fn decimate_inward(&self) -> Result<Self, AccuracyError> {
        self.decimate_inward_within(f64::INFINITY)
    }

    /// [`Self::decimate_inward`] moving the boundary by at most `deviation_mm`.
    pub(crate) fn decimate_inward_within(&self, deviation_mm: f64) -> Result<Self, AccuracyError> {
        let inherited = self.uncertainty_mm + numerical_error(self.bbox);
        let deviation_mm = deviation_mm.min(self.budget().allowance(inherited)?);
        let decimated = decimate_rings_inward(self.rings(), deviation_mm);
        Ok(Self::from_regularized(
            flatten_shapes(regularize(decimated, OverlayFillRule::Positive)),
            self.resolution,
            inherited + deviation_mm,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::ring_edges;
    use super::super::tests::res;
    use super::*;
    use crate::geom::{GeometryAccuracy, Resolution, shapes, tol};

    #[test]
    fn inward_decimation_only_shrinks_and_respects_deviation() {
        let spur = vec![[0.0, 0.0], [2.0, 0.0], [1.0, 1e-12], [1.0, 1.0], [0.0, 1.0]];
        assert!(decimate_ring_inward(&spur, 1e-10).contains(&[2.0, 0.0]));
        let ring = ContourSet::from_contours(
            &[shapes::circle(10.0).unwrap(), shapes::circle(6.0).unwrap()],
            FillRule::EvenOdd,
            res(tol::REGION_MM),
        )
        .unwrap();
        let decimated = ring.decimate_inward().unwrap();
        let deviation = decimated.uncertainty_mm - ring.uncertainty_mm;
        assert!(deviation > 0.0);

        assert!(decimated.difference(&ring).unwrap().area() <= ring.tolerance().powi(2));

        // Area loss is bounded by the deviation times the boundary length.
        let perimeter: f64 = ring
            .rings
            .iter()
            .flat_map(ring_edges)
            .map(|(start, end)| start.distance_to(end))
            .sum();
        assert!(ring.area() - decimated.area() <= deviation * perimeter);
    }

    #[test]
    fn inward_decimation_does_not_fill_a_hole_inside_the_bulge_it_cuts() {
        // The chord across the shallow bulge passes above a hole within it,
        // which leaves the hole outside its ring, wound the wrong way.
        let outer = vec![
            [0.0, 0.0],
            [5.0, -0.01],
            [10.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
        ];
        let hole = vec![[4.0, -0.002], [6.0, -0.002], [5.0, -0.008]];
        let resolution = Resolution::new(tol::REGION_MM, GeometryAccuracy::new(0.05).unwrap());
        let region = ContourSet::from_regularized(vec![outer, hole], resolution, 0.0);
        assert!((region.area() - 100.044).abs() < 1e-9);

        let decimated = region.decimate_inward().unwrap();

        assert_eq!(decimated.rings.len(), 1);
        assert!(decimated.difference(&region).unwrap().is_empty());
    }

    #[test]
    fn inward_decimation_bounds_distance_to_segment_not_line() {
        // The spike is 0.01 mm from the chord's line, but 2 mm past its end.
        let ring: Ring = vec![
            [0.0, 0.0],
            [12.0, -0.01],
            [10.0, 0.0],
            [10.0, 10.0],
            [0.0, 10.0],
        ];
        let deviation = 0.05;
        let decimated = decimate_ring_inward(&ring, deviation);

        for [x, y] in ring {
            let distance = ring_edges(&decimated)
                .map(|(a, b)| dist::point_segment(Point::new(x, y), a, b).0)
                .fold(f64::INFINITY, f64::min);
            assert!(
                distance <= deviation,
                "vertex ({x}, {y}) is {distance} mm away"
            );
        }
    }
}
