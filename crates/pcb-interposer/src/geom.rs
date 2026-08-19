//! Exact 2-D geometry for gridless smoothing and self-DRC: capsule
//! (fat segment) distances, a spatial hash of obstacles, and polyline
//! offsetting for differential pairs.

pub type P = [f64; 2];

pub fn sub(a: P, b: P) -> P {
    [a[0] - b[0], a[1] - b[1]]
}

pub fn norm(a: P) -> f64 {
    (a[0] * a[0] + a[1] * a[1]).sqrt()
}

pub fn dist(a: P, b: P) -> f64 {
    norm(sub(a, b))
}

pub fn polyline_len(pts: &[P]) -> f64 {
    pts.windows(2).map(|w| dist(w[0], w[1])).sum()
}

/// Distance from point `p` to segment `ab`.
pub fn dist_point_seg(p: P, a: P, b: P) -> f64 {
    let ab = sub(b, a);
    let l2 = ab[0] * ab[0] + ab[1] * ab[1];
    if l2 < 1e-12 {
        return dist(p, a);
    }
    let t = (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / l2).clamp(0.0, 1.0);
    dist(p, [a[0] + t * ab[0], a[1] + t * ab[1]])
}

/// Distance between segments `ab` and `cd`.
pub fn dist_seg_seg(a: P, b: P, c: P, d: P) -> f64 {
    if segs_intersect(a, b, c, d) {
        return 0.0;
    }
    dist_point_seg(a, c, d)
        .min(dist_point_seg(b, c, d))
        .min(dist_point_seg(c, a, b))
        .min(dist_point_seg(d, a, b))
}

pub fn segs_intersect(a: P, b: P, c: P, d: P) -> bool {
    fn orient(p: P, q: P, r: P) -> f64 {
        (q[0] - p[0]) * (r[1] - p[1]) - (q[1] - p[1]) * (r[0] - p[0])
    }
    let o1 = orient(a, b, c);
    let o2 = orient(a, b, d);
    let o3 = orient(c, d, a);
    let o4 = orient(c, d, b);
    o1 * o2 < 0.0 && o3 * o4 < 0.0
}

/// One obstacle in the world: a disk or a capsule, with the copper
/// radius already folded in (`r` = half-width of the copper).
#[derive(Debug, Clone, Copy)]
pub struct Obstacle {
    pub a: P,
    pub b: P,
    pub r: f64,
    /// Layer mask: bit 0 = bottom, bit 1 = top.
    pub layers: u8,
    /// Owning net (u32::MAX = fixed obstacle such as a pad or hole).
    pub owner: u32,
}

impl Obstacle {
    pub fn disk(c: P, r: f64, layers: u8, owner: u32) -> Self {
        Self {
            a: c,
            b: c,
            r,
            layers,
            owner,
        }
    }

    pub fn capsule(a: P, b: P, r: f64, layers: u8, owner: u32) -> Self {
        Self {
            a,
            b,
            r,
            layers,
            owner,
        }
    }
}

/// Spatial hash over obstacles for fast "is this fat segment clear" queries.
pub struct World {
    cell: f64,
    bins: std::collections::HashMap<(i32, i32), Vec<u32>>,
    pub obstacles: Vec<Obstacle>,
    pub w: f64,
    pub h: f64,
    pub edge_margin: f64,
}

impl World {
    pub fn new(w: f64, h: f64, edge_margin: f64) -> Self {
        Self {
            cell: 4.0,
            bins: std::collections::HashMap::new(),
            obstacles: Vec::new(),
            w,
            h,
            edge_margin,
        }
    }

    fn key(&self, p: P) -> (i32, i32) {
        (
            (p[0] / self.cell).floor() as i32,
            (p[1] / self.cell).floor() as i32,
        )
    }

    pub fn add(&mut self, ob: Obstacle) -> u32 {
        let id = self.obstacles.len() as u32;
        let pad = ob.r + self.cell;
        let (x0, y0) = self.key([ob.a[0].min(ob.b[0]) - pad, ob.a[1].min(ob.b[1]) - pad]);
        let (x1, y1) = self.key([ob.a[0].max(ob.b[0]) + pad, ob.a[1].max(ob.b[1]) + pad]);
        for gy in y0..=y1 {
            for gx in x0..=x1 {
                self.bins.entry((gx, gy)).or_default().push(id);
            }
        }
        self.obstacles.push(ob);
        id
    }

    /// True when a trace segment `ab` of half-width `r` on `layers` keeps
    /// `clear` distance from every obstacle not owned by `owner`, and stays
    /// inside the sheet.
    pub fn seg_clear(&self, a: P, b: P, r: f64, layers: u8, clear: f64, owner: u32) -> bool {
        let m = self.edge_margin + r;
        for p in [a, b] {
            if p[0] < m || p[1] < m || p[0] > self.w - m || p[1] > self.h - m {
                return false;
            }
        }
        let pad = r + clear + self.cell;
        let (x0, y0) = self.key([a[0].min(b[0]) - pad, a[1].min(b[1]) - pad]);
        let (x1, y1) = self.key([a[0].max(b[0]) + pad, a[1].max(b[1]) + pad]);
        let mut seen = std::collections::HashSet::new();
        for gy in y0..=y1 {
            for gx in x0..=x1 {
                let Some(ids) = self.bins.get(&(gx, gy)) else {
                    continue;
                };
                for &id in ids {
                    if !seen.insert(id) {
                        continue;
                    }
                    let ob = &self.obstacles[id as usize];
                    if ob.owner == owner || ob.layers & layers == 0 {
                        continue;
                    }
                    if dist_seg_seg(a, b, ob.a, ob.b) < r + ob.r + clear - 1e-9 {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Minimum gap (edge to edge) between fat segment `ab` and any obstacle
    /// not owned by `owner`, capped at `cap`. Used for DRC reporting.
    pub fn seg_min_gap(&self, a: P, b: P, r: f64, layers: u8, owner: u32, cap: f64) -> f64 {
        self.seg_min_gap_with(a, b, r, layers, owner, cap).0
    }

    /// Like `seg_min_gap`, also returning the closest obstacle's index.
    pub fn seg_min_gap_with(
        &self,
        a: P,
        b: P,
        r: f64,
        layers: u8,
        owner: u32,
        cap: f64,
    ) -> (f64, Option<u32>) {
        let pad = r + cap + self.cell;
        let (x0, y0) = self.key([a[0].min(b[0]) - pad, a[1].min(b[1]) - pad]);
        let (x1, y1) = self.key([a[0].max(b[0]) + pad, a[1].max(b[1]) + pad]);
        let mut best = cap;
        let mut hit = None;
        let mut seen = std::collections::HashSet::new();
        for gy in y0..=y1 {
            for gx in x0..=x1 {
                let Some(ids) = self.bins.get(&(gx, gy)) else {
                    continue;
                };
                for &id in ids {
                    if !seen.insert(id) {
                        continue;
                    }
                    let ob = &self.obstacles[id as usize];
                    if ob.owner == owner || ob.layers & layers == 0 {
                        continue;
                    }
                    let gap = dist_seg_seg(a, b, ob.a, ob.b) - r - ob.r;
                    if gap < best {
                        best = gap;
                        hit = Some(id);
                    }
                }
            }
        }
        (best, hit)
    }
}

/// Offset a polyline by signed distance `d` (positive = left of travel).
/// Uses miter joins; assumes gentle corners (smoothed centerlines).
pub fn offset_polyline(pts: &[P], d: f64) -> Vec<P> {
    if pts.len() < 2 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len());
    let n = pts.len();
    let normal = |a: P, b: P| -> P {
        let v = sub(b, a);
        let l = norm(v).max(1e-12);
        [-v[1] / l, v[0] / l]
    };
    for i in 0..n {
        if i == 0 {
            let nm = normal(pts[0], pts[1]);
            out.push([pts[0][0] + d * nm[0], pts[0][1] + d * nm[1]]);
        } else if i == n - 1 {
            let nm = normal(pts[n - 2], pts[n - 1]);
            out.push([pts[i][0] + d * nm[0], pts[i][1] + d * nm[1]]);
        } else {
            let v1 = sub(pts[i], pts[i - 1]);
            let v2 = sub(pts[i + 1], pts[i]);
            let n1 = normal(pts[i - 1], pts[i]);
            let n2 = normal(pts[i], pts[i + 1]);
            let turn = v1[0] * v2[1] - v1[1] * v2[0];
            // The offset must never leave the path's `|d|` envelope — the
            // routed clearances are guaranteed only for that ribbon. Inner
            // joins take the trim intersection of the two offset lines
            // (perpendicular distance stays exactly |d|); outer joins take a
            // bevel of the two exact-offset points (the chord only cuts
            // inward). A scaled miter would spike outside the envelope at
            // sharp corners.
            let inner = (d > 0.0) == (turn > 0.0);
            let a1 = [pts[i][0] + d * n1[0], pts[i][1] + d * n1[1]];
            let a2 = [pts[i][0] + d * n2[0], pts[i][1] + d * n2[1]];
            let mut trimmed = false;
            if inner && turn.abs() > 1e-9 {
                // Intersection of line(a1, dir v1) and line(a2, dir v2),
                // used only while it stays near the corner — nearly straight
                // joins would shoot it arbitrarily far along the lines.
                let w = sub(a2, a1);
                let t = (w[0] * v2[1] - w[1] * v2[0]) / turn;
                let q = [a1[0] + t * v1[0], a1[1] + t * v1[1]];
                if dist(q, pts[i]) <= 3.0 * d.abs() {
                    out.push(q);
                    trimmed = true;
                }
            }
            if !trimmed {
                // Bevel: both points sit at exactly |d|; on the inner side
                // the small same-net overlap this leaves is harmless.
                out.push(a1);
                if dist(a1, a2) > 1e-9 {
                    out.push(a2);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seg_distances() {
        assert!((dist_point_seg([0.0, 1.0], [-1.0, 0.0], [1.0, 0.0]) - 1.0).abs() < 1e-9);
        assert!((dist_seg_seg([0.0, 1.0], [1.0, 1.0], [0.0, 0.0], [1.0, 0.0]) - 1.0).abs() < 1e-9);
        assert_eq!(
            dist_seg_seg([0.0, -1.0], [0.0, 1.0], [-1.0, 0.0], [1.0, 0.0]),
            0.0
        );
    }

    #[test]
    fn world_blocks_and_clears() {
        let mut w = World::new(100.0, 100.0, 0.5);
        w.add(Obstacle::disk([50.0, 50.0], 0.5, 0b11, u32::MAX));
        assert!(!w.seg_clear([40.0, 50.0], [60.0, 50.0], 0.125, 0b01, 0.2, 0));
        assert!(w.seg_clear([40.0, 60.0], [60.0, 60.0], 0.125, 0b01, 0.2, 0));
        // Same owner is ignored.
        let id = w.add(Obstacle::capsule(
            [40.0, 55.0],
            [60.0, 55.0],
            0.125,
            0b01,
            7,
        ));
        let _ = id;
        assert!(w.seg_clear([40.0, 55.2], [60.0, 55.2], 0.125, 0b01, 0.2, 7));
        assert!(!w.seg_clear([40.0, 55.2], [60.0, 55.2], 0.125, 0b01, 0.2, 8));
    }

    #[test]
    fn offset_straight_line() {
        let pts = vec![[0.0, 0.0], [10.0, 0.0]];
        let off = offset_polyline(&pts, 1.0);
        assert!((off[0][1] - 1.0).abs() < 1e-9 && (off[1][1] - 1.0).abs() < 1e-9);
    }
}
