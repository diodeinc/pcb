//! Pure tab-set selection: the fewest sites that keep a board's worst-case
//! deflection under a process load within a limit.
//!
//! Two mechanisms carry a point load on a board held by tabs. The board moves
//! as a rigid plate on its tabs, each tab a short beam between board and
//! rail with transverse, bending and twisting stiffness from its own
//! geometry, in series with the twist and bending of the rail it lands on;
//! a 3x3 solve gives that motion, so one tab is a soft cantilever hinge, two
//! collinear tabs a stiffer one, and three spread tabs rigid. And the board
//! bends locally between the load and its nearest tab, taken as a cantilever
//! strip whose effective width grows with the distance. Nothing here counts
//! tabs or sides; the count follows from thickness and size. Units are N
//! and mm.

use pcb_ir::geom::Point;

/// Equivalent isotropic laminate, N/mm².
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Laminate {
    pub youngs_modulus: f64,
    pub shear_modulus: f64,
    pub poisson_ratio: f64,
}

/// One tab as a beam of `width` and `length` between board and frame, with
/// its section reduced by perforation.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct TabBeam {
    pub width_mm: f64,
    pub length_mm: f64,
    pub perforation_factor: f64,
}

/// The frame rail a tab lands on: a strip of the board's thickness, held
/// where cross rails meet it, one board span apart.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Rail {
    pub width_mm: f64,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Process {
    /// Point load that may act anywhere on the board, N.
    pub load_n: f64,
    /// Allowed deflection under that load, mm.
    pub deflection_limit_mm: f64,
}

/// Derived stiffnesses for one board thickness and rail span.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Model {
    pub thickness_mm: f64,
    /// Distance between the cross rails holding the rail a tab lands on.
    pub rail_span_mm: f64,
    /// Plate flexural rigidity D = E t³ / 12(1 − ν²), N·mm.
    pub rigidity_n_mm: f64,
    /// Stiffness against board displacement across the slot, N/mm.
    pub tab_transverse_n_per_mm: f64,
    /// Stiffness against board rotation about the edge: neck bending in
    /// series with rail twist, N·mm/rad.
    pub tab_bending_n_mm: f64,
    /// Stiffness against board rotation about the tab axis: neck twist in
    /// series with rail bending, N·mm/rad.
    pub tab_twist_n_mm: f64,
    pub load_n: f64,
    pub deflection_limit_mm: f64,
    /// Farthest a load may sit from any tab before local bending alone
    /// exceeds the limit; the search prunes with it.
    pub reach_mm: f64,
    /// Manufacturing: closest two tabs may sit.
    pub min_separation_mm: f64,
}

/// A point load `d` from the nearest tab bends the board like a cantilever
/// strip about `2d` wide: F d³ / (3 D · 2d) = F d² / (6 D).
const BENDING_SPREAD: f64 = 6.0;

impl Model {
    pub fn new(
        thickness_mm: f64,
        rail_span_mm: f64,
        laminate: Laminate,
        tab: TabBeam,
        rail: Rail,
        process: Process,
        min_separation_mm: f64,
    ) -> Self {
        let (e, g, nu) = (
            laminate.youngs_modulus,
            laminate.shear_modulus,
            laminate.poisson_ratio,
        );
        let (w, l, t) = (tab.width_mm, tab.length_mm, thickness_mm);
        let f = tab.perforation_factor;
        let neck_inertia = w * t.powi(3) / 12.0;
        let neck_transverse = f * 12.0 * e * neck_inertia / l.powi(3);
        let neck_bending = f * 4.0 * e * neck_inertia / l;
        let neck_twist = f * g * torsion_constant(w, t) / l;
        // A torque or moment applied at mid-span of a rail held at both ends.
        let rail_twist = 4.0 * g * torsion_constant(rail.width_mm, t) / rail_span_mm;
        let rail_bending = 12.0 * e * rail.width_mm * t.powi(3) / 12.0 / rail_span_mm;
        let rigidity_n_mm = e * t.powi(3) / (12.0 * (1.0 - nu * nu));
        Self {
            thickness_mm,
            rail_span_mm,
            rigidity_n_mm,
            tab_transverse_n_per_mm: neck_transverse,
            tab_bending_n_mm: series(neck_bending, rail_twist),
            tab_twist_n_mm: series(neck_twist, rail_bending),
            load_n: process.load_n,
            deflection_limit_mm: process.deflection_limit_mm,
            reach_mm: (BENDING_SPREAD * rigidity_n_mm * process.deflection_limit_mm
                / process.load_n)
                .sqrt(),
            min_separation_mm,
        }
    }
}

/// Saint-Venant torsion constant of a rectangular section.
fn torsion_constant(a: f64, b: f64) -> f64 {
    let (wide, thin) = (a.max(b), a.min(b));
    wide * thin.powi(3) / 3.0 * (1.0 - 0.63 * thin / wide)
}

fn series(a: f64, b: f64) -> f64 {
    a * b / (a + b)
}

#[derive(Debug, Clone, Copy)]
pub struct Site {
    pub point: Point,
    pub outward_normal: Point,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Violation {
    NoSites,
    Deflection { deflection_mm: f64, limit_mm: f64 },
    Separation { distance_mm: f64, minimum_mm: f64 },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSites => write!(f, "no candidate sites on the outline"),
            Self::Deflection {
                deflection_mm,
                limit_mm,
            } => write!(f, "deflects {deflection_mm:.2} mm, limit {limit_mm:.2}"),
            Self::Separation {
                distance_mm,
                minimum_mm,
            } => write!(f, "tabs {distance_mm:.1} mm apart, minimum {minimum_mm:.1}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Selection {
    pub chosen: Vec<usize>,
    /// Worst deflection over the load points under the process load.
    pub deflection_mm: f64,
    /// Index of the load point that deflects most.
    pub worst_point: Option<usize>,
    pub violations: Vec<Violation>,
}

impl Selection {
    pub fn satisfied(&self) -> bool {
        self.violations.is_empty()
    }

    fn better_than(&self, other: &Self) -> bool {
        (self.violations.len(), self.deflection_mm) < (other.violations.len(), other.deflection_mm)
    }
}

/// Fewest tabs whose worst deflection is within the limit. A greedy pass
/// with pruning and swapping gives a feasible set and an upper bound; the
/// exhaustive search then looks for anything smaller, up to four tabs and as
/// long as the enumeration stays affordable. Always returns a set; check
/// `violations`.
pub fn select(sites: &[Site], loads: &[Point], model: &Model) -> Selection {
    if sites.is_empty() {
        return Selection {
            chosen: Vec::new(),
            deflection_mm: f64::INFINITY,
            worst_point: None,
            violations: vec![Violation::NoSites],
        };
    }
    let evaluator = Evaluator::new(sites, loads, model);
    let n = sites.len();
    let greedy = evaluator.greedy();
    let bound = if greedy.satisfied() {
        greedy.chosen.len()
    } else {
        n.min(4) + 1
    };
    for k in 1..bound.min(5) {
        if combinations(n, k) > EXHAUSTIVE_BUDGET {
            break;
        }
        let mut best: Option<Selection> = None;
        evaluator.search(k, &mut Vec::with_capacity(k), &mut |chosen| {
            let candidate = evaluator.evaluate(chosen);
            if candidate.satisfied() && best.as_ref().is_none_or(|b| candidate.better_than(b)) {
                best = Some(candidate);
            }
        });
        if let Some(best) = best {
            return best;
        }
    }
    greedy
}

/// Subsets the exhaustive phase may enumerate per tab count.
const EXHAUSTIVE_BUDGET: f64 = 3.0e6;

fn combinations(n: usize, k: usize) -> f64 {
    (0..k).fold(1.0, |acc, i| acc * (n - i) as f64 / (i + 1) as f64)
}

#[derive(Clone, Copy)]
enum Move {
    Add,
    Drop,
    Swap,
}

struct Evaluator<'a> {
    sites: &'a [Site],
    model: &'a Model,
    /// Load points relative to their centroid, as rigid-plane rows [1, x, y].
    rows: Vec<[f64; 3]>,
    /// Sites relative to the same centroid.
    offsets: Vec<Point>,
    /// Squared distance from each site to each load point.
    distance2: Vec<Vec<f64>>,
    /// Per site, the load points within reach, as bit words.
    within_reach: Vec<Vec<u64>>,
    all_points: Vec<u64>,
}

impl<'a> Evaluator<'a> {
    fn new(sites: &'a [Site], loads: &'a [Point], model: &'a Model) -> Self {
        let centroid = loads.iter().fold(Point::ZERO, |c, p| c + *p) / loads.len().max(1) as f64;
        let rows = loads
            .iter()
            .map(|p| [1.0, p.x - centroid.x, p.y - centroid.y])
            .collect();
        let offsets = sites.iter().map(|s| s.point - centroid).collect();
        let distance2: Vec<Vec<f64>> = sites
            .iter()
            .map(|site| {
                loads
                    .iter()
                    .map(|p| {
                        let d = *p - site.point;
                        d.x * d.x + d.y * d.y
                    })
                    .collect()
            })
            .collect();
        let words = loads.len().div_ceil(64);
        let reach2 = model.reach_mm * model.reach_mm;
        let within_reach = distance2
            .iter()
            .map(|row| {
                let mut bits = vec![0u64; words];
                for (j, d2) in row.iter().enumerate() {
                    if *d2 <= reach2 {
                        bits[j / 64] |= 1 << (j % 64);
                    }
                }
                bits
            })
            .collect();
        let mut all_points = vec![u64::MAX; words];
        if let Some(last) = all_points.last_mut()
            && !loads.len().is_multiple_of(64)
        {
            *last = (1u64 << (loads.len() % 64)) - 1;
        }
        Self {
            sites,
            model,
            rows,
            offsets,
            distance2,
            within_reach,
            all_points,
        }
    }

    /// Best pair, then add whichever site helps most while adding still
    /// helps, then drop and swap tabs while that keeps the limit and lowers
    /// the deflection.
    fn greedy(&self) -> Selection {
        let n = self.sites.len();
        let mut incumbent = (0..n)
            .flat_map(|a| (a + 1..n).map(move |b| vec![a, b]))
            .map(|pair| self.evaluate(&pair))
            .reduce(|a, b| if b.better_than(&a) { b } else { a })
            .unwrap_or_else(|| self.evaluate(&[0]));
        while !incumbent.satisfied() {
            match self.best_neighbor(&incumbent, Move::Add) {
                Some(better) if better.better_than(&incumbent) => incumbent = better,
                _ => break,
            }
        }
        loop {
            let pruned = self
                .best_neighbor(&incumbent, Move::Drop)
                .filter(|s| s.satisfied());
            if let Some(pruned) = pruned {
                incumbent = pruned;
                continue;
            }
            match self.best_neighbor(&incumbent, Move::Swap) {
                Some(swapped) if swapped.satisfied() && swapped.better_than(&incumbent) => {
                    incumbent = swapped
                }
                _ => break,
            }
        }
        incumbent
    }

    /// The best selection one move away from `from`.
    fn best_neighbor(&self, from: &Selection, kind: Move) -> Option<Selection> {
        let n = self.sites.len();
        let unused = || (0..n).filter(|i| !from.chosen.contains(i));
        let candidates: Vec<Vec<usize>> = match kind {
            Move::Add => unused()
                .map(|i| {
                    let mut chosen = from.chosen.clone();
                    chosen.push(i);
                    chosen.sort_unstable();
                    chosen
                })
                .collect(),
            Move::Drop => (0..from.chosen.len())
                .map(|k| {
                    let mut chosen = from.chosen.clone();
                    chosen.remove(k);
                    chosen
                })
                .collect(),
            Move::Swap => (0..from.chosen.len())
                .flat_map(|k| {
                    unused().map(move |i| {
                        let mut chosen = from.chosen.clone();
                        chosen[k] = i;
                        chosen.sort_unstable();
                        chosen
                    })
                })
                .collect(),
        };
        // A neighbor whose running maximum already exceeds the best one found
        // cannot win, so evaluation stops there; starting from the incumbent's
        // worst point makes that happen early.
        let start = from.worst_point.unwrap_or(0);
        let mut best: Option<Selection> = None;
        for chosen in candidates.iter().filter(|chosen| !chosen.is_empty()) {
            let (bound, crowded) = best.as_ref().map_or((f64::INFINITY, 0), |b| {
                (
                    b.deflection_mm,
                    b.violations.len()
                        - usize::from(b.deflection_mm > self.model.deflection_limit_mm),
                )
            });
            if let Some(candidate) = self.evaluate_bounded(chosen, start, bound, crowded)
                && best.as_ref().is_none_or(|b| candidate.better_than(b))
            {
                best = Some(candidate);
            }
        }
        best
    }

    /// Visit every `k`-subset whose tabs are separated and whose local bending
    /// alone stays within the limit.
    fn search(&self, k: usize, chosen: &mut Vec<usize>, visit: &mut impl FnMut(&[usize])) {
        if chosen.len() == k {
            if self.all_within_reach(chosen) {
                visit(chosen);
            }
            return;
        }
        let start = chosen.last().map_or(0, |&i| i + 1);
        for i in start..=self.sites.len() - (k - chosen.len()) {
            let separated = chosen.iter().all(|&j| {
                self.sites[i].point.distance_to(self.sites[j].point) >= self.model.min_separation_mm
            });
            if separated {
                chosen.push(i);
                self.search(k, chosen, visit);
                chosen.pop();
            }
        }
    }

    fn all_within_reach(&self, chosen: &[usize]) -> bool {
        self.all_points.iter().enumerate().all(|(w, &all)| {
            chosen
                .iter()
                .fold(0u64, |acc, &i| acc | self.within_reach[i][w])
                & all
                == all
        })
    }

    fn evaluate(&self, chosen: &[usize]) -> Selection {
        self.evaluate_bounded(chosen, 0, f64::INFINITY, 0)
            .expect("an unbounded evaluation always completes")
    }

    /// Evaluate `chosen`, scanning load points from `start`. Give up with
    /// `None` once the deflection exceeds `bound`, unless this set has fewer
    /// separation violations than the `crowded` count the bound came with,
    /// since violations rank before deflection.
    fn evaluate_bounded(
        &self,
        chosen: &[usize],
        start: usize,
        bound: f64,
        crowded: usize,
    ) -> Option<Selection> {
        let model = self.model;
        let mut violations = Vec::new();
        let separation = chosen
            .iter()
            .enumerate()
            .flat_map(|(i, &a)| chosen[i + 1..].iter().map(move |&b| (a, b)))
            .map(|(a, b)| self.sites[a].point.distance_to(self.sites[b].point))
            .fold(f64::INFINITY, f64::min);
        if separation < model.min_separation_mm {
            violations.push(Violation::Separation {
                distance_mm: separation,
                minimum_mm: model.min_separation_mm,
            });
        }
        let compliance = self.rigid_compliance(chosen);
        let (mut worst_point, mut deflection_mm) = (None, 0.0);
        let count = self.rows.len();
        for j in (start..count).chain(0..start) {
            let rigid = quadratic_form(&compliance, &self.rows[j]);
            let nearest2 = chosen
                .iter()
                .map(|&i| self.distance2[i][j])
                .fold(f64::INFINITY, f64::min);
            let d = model.load_n * (rigid + nearest2 / (BENDING_SPREAD * model.rigidity_n_mm));
            if d > deflection_mm {
                (worst_point, deflection_mm) = (Some(j), d);
                if d > bound && violations.len() >= crowded {
                    return None;
                }
            }
        }
        if deflection_mm > model.deflection_limit_mm {
            violations.push(Violation::Deflection {
                deflection_mm,
                limit_mm: model.deflection_limit_mm,
            });
        }
        Some(Selection {
            chosen: chosen.to_vec(),
            deflection_mm,
            worst_point,
            violations,
        })
    }

    /// Inverse stiffness of the rigid plane `[w, ∂w/∂x, ∂w/∂y]` on the chosen
    /// tabs: transverse springs at each tab, bending springs on the slope
    /// along each tab's normal, twist springs on the slope along its tangent.
    fn rigid_compliance(&self, chosen: &[usize]) -> [[f64; 3]; 3] {
        let model = self.model;
        let mut k = [[0.0; 3]; 3];
        for &i in chosen {
            let p = self.offsets[i];
            let n = self.sites[i].outward_normal;
            let t = Point::new(-n.y, n.x);
            for (row, stiffness) in [
                ([1.0, p.x, p.y], model.tab_transverse_n_per_mm),
                ([0.0, n.x, n.y], model.tab_bending_n_mm),
                ([0.0, t.x, t.y], model.tab_twist_n_mm),
            ] {
                for a in 0..3 {
                    for b in 0..3 {
                        k[a][b] += stiffness * row[a] * row[b];
                    }
                }
            }
        }
        invert_symmetric(k).unwrap_or([[f64::INFINITY; 3]; 3])
    }
}

fn quadratic_form(m: &[[f64; 3]; 3], v: &[f64; 3]) -> f64 {
    (0..3)
        .map(|a| v[a] * (0..3).map(|b| m[a][b] * v[b]).sum::<f64>())
        .sum()
}

fn invert_symmetric(m: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let [[a, b, c], [_, d, e], [_, _, f]] = m;
    let det = a * (d * f - e * e) - b * (b * f - c * e) + c * (b * e - c * d);
    if !det.is_finite() || det.abs() <= f64::MIN_POSITIVE {
        return None;
    }
    let cofactor = [
        [d * f - e * e, c * e - b * f, b * e - c * d],
        [c * e - b * f, a * f - c * c, b * c - a * e],
        [b * e - c * d, b * c - a * e, a * d - b * b],
    ];
    Some(cofactor.map(|row| row.map(|x| x / det)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(thickness_mm: f64) -> Model {
        Model::new(
            thickness_mm,
            60.0,
            Laminate {
                youngs_modulus: 22_000.0,
                shear_modulus: 4_500.0,
                poisson_ratio: 0.13,
            },
            TabBeam {
                width_mm: 2.0,
                length_mm: 2.0,
                perforation_factor: 0.5,
            },
            Rail { width_mm: 6.0 },
            Process {
                load_n: 5.0,
                deflection_limit_mm: 0.25,
            },
            10.0,
        )
    }

    /// Rectangle `w` by `h` at the origin, sampled every mm, with candidate
    /// sites every `pitch` on all four sides.
    fn rectangle(w: f64, h: f64, pitch: f64) -> (Vec<Point>, Vec<Site>) {
        let mut outline = Vec::new();
        let mut sites = Vec::new();
        let sides = [
            (
                Point::new(0.0, 0.0),
                Point::new(1.0, 0.0),
                Point::new(0.0, -1.0),
                w,
            ),
            (
                Point::new(w, 0.0),
                Point::new(0.0, 1.0),
                Point::new(1.0, 0.0),
                h,
            ),
            (
                Point::new(w, h),
                Point::new(-1.0, 0.0),
                Point::new(0.0, 1.0),
                w,
            ),
            (
                Point::new(0.0, h),
                Point::new(0.0, -1.0),
                Point::new(-1.0, 0.0),
                h,
            ),
        ];
        for (start, tangent, normal, length) in sides {
            let mut s = 0.0;
            while s < length {
                outline.push(start + tangent * s);
                s += 1.0;
            }
            let mut s = pitch / 2.0;
            while s < length {
                sites.push(Site {
                    point: start + tangent * s,
                    outward_normal: normal,
                });
                s += pitch;
            }
        }
        (outline, sites)
    }

    fn tabs(w: f64, h: f64, thickness_mm: f64) -> Selection {
        let (outline, sites) = rectangle(w, h, 5.0);
        select(&sites, &outline, &model(thickness_mm))
    }

    #[test]
    fn stiffnesses_follow_the_beam_formulas() {
        let m = model(1.6);
        let inertia = 2.0 * 1.6f64.powi(3) / 12.0;
        assert!((m.tab_transverse_n_per_mm - 0.5 * 12.0 * 22_000.0 * inertia / 8.0).abs() < 1e-6);
        // The rail's twist limits the neck's bending stiffness.
        let neck_bending = 0.5 * 4.0 * 22_000.0 * inertia / 2.0;
        assert!(m.tab_bending_n_mm < 0.25 * neck_bending && m.tab_bending_n_mm > 0.0);
        assert!(m.tab_twist_n_mm > 0.0);
        let rigidity = 22_000.0 * 1.6f64.powi(3) / (12.0 * (1.0 - 0.13 * 0.13));
        assert!((m.rigidity_n_mm - rigidity).abs() < 1e-6);
    }

    #[test]
    fn one_tab_is_a_finite_but_soft_cantilever() {
        let (outline, sites) = rectangle(40.0, 20.0, 5.0);
        let m = model(1.6);
        let one = Evaluator::new(&sites, &outline, &m).evaluate(&[0]);
        assert!(one.deflection_mm.is_finite());
        assert!(one.deflection_mm > m.deflection_limit_mm);
    }

    #[test]
    fn count_grows_with_board_size() {
        let small = tabs(20.0, 10.0, 1.6);
        let medium = tabs(60.0, 30.0, 1.6);
        let long = tabs(250.0, 30.0, 1.6);
        assert!(small.satisfied() && medium.satisfied() && long.satisfied());
        assert!(small.chosen.len() <= 2, "{:?}", small.chosen);
        assert!(medium.chosen.len() >= 3);
        assert!(long.chosen.len() > medium.chosen.len());
    }

    #[test]
    fn thinner_boards_take_more_tabs() {
        let thick = tabs(120.0, 60.0, 1.6);
        let thin = tabs(120.0, 60.0, 0.8);
        assert!(thick.satisfied() && thin.satisfied());
        assert!(
            thin.chosen.len() > thick.chosen.len(),
            "{} vs {}",
            thin.chosen.len(),
            thick.chosen.len()
        );
    }

    #[test]
    fn opposed_pair_hinges_where_a_triangle_holds() {
        let (outline, sites) = rectangle(60.0, 30.0, 5.0);
        let m = model(1.6);
        let evaluator = Evaluator::new(&sites, &outline, &m);
        let at = |x: f64, y: f64| {
            sites
                .iter()
                .position(|s| (s.point.x - x).abs() < 1e-9 && (s.point.y - y).abs() < 1e-9)
                .unwrap()
        };
        let opposed = evaluator.evaluate(&[at(32.5, 0.0), at(32.5, 30.0)]);
        let same_edge = evaluator.evaluate(&[at(12.5, 0.0), at(47.5, 0.0)]);
        let triangle = evaluator.evaluate(&[at(32.5, 0.0), at(2.5, 30.0), at(57.5, 30.0)]);
        assert!(opposed.deflection_mm > 3.0 * triangle.deflection_mm);
        assert!(same_edge.deflection_mm > 3.0 * triangle.deflection_mm);
    }

    #[test]
    fn crowded_candidates_report_separation_instead_of_failing() {
        let (outline, sites) = rectangle(60.0, 30.0, 5.0);
        let crowded: Vec<_> = sites
            .iter()
            .copied()
            .filter(|s| s.point.x < 12.0 && s.point.y == 0.0)
            .collect();
        let selection = select(&crowded, &outline, &model(1.6));
        assert!(!selection.chosen.is_empty());
        assert!(!selection.satisfied());
    }
}
