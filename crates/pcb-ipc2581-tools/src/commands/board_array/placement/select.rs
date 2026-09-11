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

/// Material, tab and process inputs the model is derived from.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Physics {
    /// Equivalent isotropic laminate, N/mm².
    pub youngs_modulus_mpa: f64,
    pub shear_modulus_mpa: f64,
    pub poisson_ratio: f64,
    /// The neck is a beam of this width between board and rail, its section
    /// reduced by perforation.
    pub neck_width_mm: f64,
    pub neck_perforation_factor: f64,
    /// The rail a tab lands on: a strip of the board's thickness, held where
    /// cross rails meet it.
    pub rail_width_mm: f64,
    /// Point load that may act anywhere on the board, N.
    pub load_n: f64,
    /// Allowed deflection under that load, mm.
    pub deflection_limit_mm: f64,
}

/// Stiffnesses derived for one board's thickness, span and slot.
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
        neck_length_mm: f64,
        physics: Physics,
        min_separation_mm: f64,
    ) -> Self {
        let Physics {
            youngs_modulus_mpa: e,
            shear_modulus_mpa: g,
            poisson_ratio: nu,
            neck_width_mm: w,
            neck_perforation_factor: f,
            rail_width_mm,
            load_n,
            deflection_limit_mm,
        } = physics;
        let (t, l) = (thickness_mm, neck_length_mm);
        let neck_inertia = w * t.powi(3) / 12.0;
        let rail_inertia = rail_width_mm * t.powi(3) / 12.0;
        // Neck as a beam guided at both ends; rail loaded at mid-span between
        // its supports, by a torque for the neck's bending and by a moment
        // for the neck's twist.
        let neck_transverse = f * 12.0 * e * neck_inertia / l.powi(3);
        let neck_bending = f * 4.0 * e * neck_inertia / l;
        let neck_twist = f * g * torsion_constant(w, t) / l;
        let rail_twist = 4.0 * g * torsion_constant(rail_width_mm, t) / rail_span_mm;
        let rail_bending = 12.0 * e * rail_inertia / rail_span_mm;
        let rigidity_n_mm = e * t.powi(3) / (12.0 * (1.0 - nu * nu));
        Self {
            thickness_mm,
            rail_span_mm,
            rigidity_n_mm,
            tab_transverse_n_per_mm: neck_transverse,
            tab_bending_n_mm: series(neck_bending, rail_twist),
            tab_twist_n_mm: series(neck_twist, rail_bending),
            load_n,
            deflection_limit_mm,
            reach_mm: (BENDING_SPREAD * rigidity_n_mm * deflection_limit_mm / load_n).sqrt(),
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
    NoTabs,
    Deflection { deflection_mm: f64, limit_mm: f64 },
    Separation { distance_mm: f64, minimum_mm: f64 },
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTabs => write!(f, "no candidate sites on the outline"),
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
    /// Whether every smaller set was exhaustively ruled out.
    pub proven: bool,
}

impl Selection {
    pub fn satisfied(&self) -> bool {
        self.violations.is_empty()
    }

    /// Fewer violations first, then less deflection.
    fn better_than(&self, other: &Self) -> bool {
        (self.violations.len(), self.deflection_mm) < (other.violations.len(), other.deflection_mm)
    }
}

/// Exhaustive search stops after this many tabs.
const EXHAUSTIVE_TABS: usize = 4;
/// Subsets the exhaustive phase may enumerate per tab count.
const EXHAUSTIVE_BUDGET: f64 = 3.0e6;

/// Fewest tabs whose worst deflection is within the limit. A greedy pass
/// with pruning and swapping gives a feasible set; the exhaustive search
/// then finds the best set no larger than it, smallest count first, up to
/// [`EXHAUSTIVE_TABS`] and while the enumeration stays within budget, which
/// is what `proven` records. Always returns a set; check `violations`.
pub fn select(sites: &[Site], loads: &[Point], model: &Model) -> Selection {
    let evaluator = Evaluator::new(sites, loads, model);
    let mut greedy = evaluator.greedy();
    let largest = if greedy.satisfied() {
        greedy.chosen.len()
    } else {
        EXHAUSTIVE_TABS
    };
    for k in 1..=largest.min(EXHAUSTIVE_TABS).min(sites.len()) {
        if combinations(sites.len(), k) > EXHAUSTIVE_BUDGET {
            return greedy;
        }
        if let Some(mut best) = evaluator.exhaustive(k) {
            best.proven = true;
            return best;
        }
    }
    greedy.proven = greedy.satisfied() && greedy.chosen.len() <= EXHAUSTIVE_TABS + 1;
    greedy
}

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
    /// Distance between each pair of sites.
    spacing: Vec<Vec<f64>>,
    /// Squared distance from each site to each load point.
    distance2: Vec<Vec<f64>>,
    /// Per site, the load points within reach, as bit words.
    within_reach: Vec<Vec<u64>>,
    all_points: Vec<u64>,
}

impl<'a> Evaluator<'a> {
    fn new(sites: &'a [Site], loads: &'a [Point], model: &'a Model) -> Self {
        let centroid = loads.iter().fold(Point::ZERO, |c, p| c + *p) / loads.len().max(1) as f64;
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
            rows: loads
                .iter()
                .map(|p| [1.0, p.x - centroid.x, p.y - centroid.y])
                .collect(),
            offsets: sites.iter().map(|s| s.point - centroid).collect(),
            spacing: sites
                .iter()
                .map(|a| sites.iter().map(|b| a.point.distance_to(b.point)).collect())
                .collect(),
            distance2,
            within_reach,
            all_points,
        }
    }

    /// Best separated pair, then add whichever site helps most while adding
    /// still helps, then drop and swap tabs while that keeps the limit and
    /// lowers the deflection. Moves only ever offer sites clear of the tabs
    /// kept, so a separation violation never has to be repaired.
    fn greedy(&self) -> Selection {
        let n = self.sites.len();
        let mut incumbent = (0..n)
            .flat_map(|a| (a + 1..n).map(move |b| vec![a, b]))
            .filter(|pair| self.clear(pair[0], &pair[1..]))
            .map(|pair| self.evaluate(&pair))
            .reduce(|a, b| if b.better_than(&a) { b } else { a })
            .unwrap_or_else(|| self.evaluate(&[]));
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

    /// Whether `site` is unused by and separated from every tab in `kept`.
    fn clear(&self, site: usize, kept: &[usize]) -> bool {
        kept.iter()
            .all(|&j| j != site && self.spacing[site][j] >= self.model.min_separation_mm)
    }

    /// The best selection one move away from `from`. A neighbor whose running
    /// maximum already exceeds the best one found cannot win, so its
    /// evaluation stops there; starting from the incumbent's worst point
    /// makes that happen early.
    fn best_neighbor(&self, from: &Selection, kind: Move) -> Option<Selection> {
        let n = self.sites.len();
        let k = from.chosen.len();
        let neighbors: Vec<Vec<usize>> = match kind {
            Move::Add => (0..n)
                .filter(|&i| self.clear(i, &from.chosen))
                .map(|i| with(&from.chosen, k, i))
                .collect(),
            Move::Drop => (0..k).map(|slot| without(&from.chosen, slot)).collect(),
            Move::Swap => (0..k)
                .flat_map(|slot| {
                    let kept = without(&from.chosen, slot);
                    (0..n)
                        .filter(|&i| self.clear(i, &kept))
                        .map(|i| with(&kept, slot, i))
                        .collect::<Vec<_>>()
                })
                .collect(),
        };
        let start = from.worst_point.unwrap_or(0);
        let mut best: Option<Selection> = None;
        for chosen in &neighbors {
            let bound = best.as_ref().map_or(f64::INFINITY, |b| b.deflection_mm);
            if let Some(candidate) = self.evaluate_bounded(chosen, start, bound)
                && best.as_ref().is_none_or(|b| candidate.better_than(b))
            {
                best = Some(candidate);
            }
        }
        best
    }

    /// Best rule-satisfying `k`-subset, enumerated with separation pruning
    /// and skipping sets whose local bending alone exceeds the limit.
    fn exhaustive(&self, k: usize) -> Option<Selection> {
        let mut best: Option<Selection> = None;
        self.search(k, &mut Vec::with_capacity(k), &mut |chosen| {
            let candidate = self.evaluate(chosen);
            if candidate.satisfied() && best.as_ref().is_none_or(|b| candidate.better_than(b)) {
                best = Some(candidate);
            }
        });
        best
    }

    fn search(&self, k: usize, chosen: &mut Vec<usize>, visit: &mut impl FnMut(&[usize])) {
        if chosen.len() == k {
            if self.all_within_reach(chosen) {
                visit(chosen);
            }
            return;
        }
        let start = chosen.last().map_or(0, |&i| i + 1);
        for i in start..=self.sites.len() - (k - chosen.len()) {
            if self.clear(i, chosen) {
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
        self.evaluate_bounded(chosen, 0, f64::INFINITY)
            .expect("an unbounded evaluation always completes")
    }

    /// Evaluate `chosen`, scanning load points from `start`, and give up with
    /// `None` once the deflection exceeds `bound`. Callers compare sets with
    /// the same separation state, so a larger deflection can never rank
    /// better.
    fn evaluate_bounded(&self, chosen: &[usize], start: usize, bound: f64) -> Option<Selection> {
        let model = self.model;
        let mut selection = Selection {
            chosen: chosen.to_vec(),
            deflection_mm: f64::INFINITY,
            worst_point: None,
            violations: Vec::new(),
            proven: false,
        };
        let Some(compliance) = self.rigid_compliance(chosen) else {
            selection.violations.push(Violation::NoTabs);
            return Some(selection);
        };
        let separation = chosen
            .iter()
            .enumerate()
            .flat_map(|(i, &a)| chosen[i + 1..].iter().map(move |&b| self.spacing[a][b]))
            .fold(f64::INFINITY, f64::min);
        if separation < model.min_separation_mm {
            selection.violations.push(Violation::Separation {
                distance_mm: separation,
                minimum_mm: model.min_separation_mm,
            });
        }
        selection.deflection_mm = 0.0;
        let count = self.rows.len();
        for j in (start..count).chain(0..start) {
            let rigid = quadratic_form(&compliance, &self.rows[j]);
            let nearest2 = chosen
                .iter()
                .map(|&i| self.distance2[i][j])
                .fold(f64::INFINITY, f64::min);
            let d = model.load_n * (rigid + nearest2 / (BENDING_SPREAD * model.rigidity_n_mm));
            if d > selection.deflection_mm {
                (selection.worst_point, selection.deflection_mm) = (Some(j), d);
                if d > bound {
                    return None;
                }
            }
        }
        if selection.deflection_mm > model.deflection_limit_mm {
            selection.violations.push(Violation::Deflection {
                deflection_mm: selection.deflection_mm,
                limit_mm: model.deflection_limit_mm,
            });
        }
        Some(selection)
    }

    /// Inverse stiffness of the rigid plane `[w, ∂w/∂x, ∂w/∂y]` on the chosen
    /// tabs: transverse springs at each tab, bending springs on the slope
    /// along each tab's normal, twist springs on the slope along its tangent.
    /// `None` without tabs, when the plane is free.
    fn rigid_compliance(&self, chosen: &[usize]) -> Option<[[f64; 3]; 3]> {
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
        invert_symmetric(k)
    }
}

/// `chosen` with `site` inserted at `slot`.
fn with(chosen: &[usize], slot: usize, site: usize) -> Vec<usize> {
    let mut next = chosen.to_vec();
    next.insert(slot, site);
    next.sort_unstable();
    next
}

/// `chosen` without the entry at `slot`.
fn without(chosen: &[usize], slot: usize) -> Vec<usize> {
    let mut next = chosen.to_vec();
    next.remove(slot);
    next
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

    const FR4: Physics = Physics {
        youngs_modulus_mpa: 22_000.0,
        shear_modulus_mpa: 4_500.0,
        poisson_ratio: 0.13,
        neck_width_mm: 2.0,
        neck_perforation_factor: 0.5,
        rail_width_mm: 6.0,
        load_n: 5.0,
        deflection_limit_mm: 0.25,
    };

    fn model(thickness_mm: f64) -> Model {
        Model::new(thickness_mm, 60.0, 2.0, FR4, 10.0)
    }

    /// Rectangle `w` by `h` at the origin, sampled every mm along the outline,
    /// with candidate sites every `pitch` on all four sides.
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
    fn no_sites_and_one_tab_are_reported_not_solved() {
        let (outline, sites) = rectangle(40.0, 20.0, 5.0);
        let m = model(1.6);
        let none = select(&[], &outline, &m);
        assert_eq!(none.violations, vec![Violation::NoTabs]);
        assert!(none.chosen.is_empty() && !none.proven);
        let one = Evaluator::new(&sites, &outline, &m).evaluate(&[0]);
        assert!(one.deflection_mm.is_finite());
        assert!(one.deflection_mm > m.deflection_limit_mm);
    }

    #[test]
    fn count_grows_with_board_size_and_small_counts_are_proven() {
        let small = tabs(20.0, 10.0, 1.6);
        let medium = tabs(60.0, 30.0, 1.6);
        let long = tabs(250.0, 30.0, 1.6);
        assert!(small.satisfied() && medium.satisfied() && long.satisfied());
        assert!(small.chosen.len() <= 2, "{:?}", small.chosen);
        assert!(medium.chosen.len() >= 3);
        assert!(long.chosen.len() > medium.chosen.len());
        assert!(small.proven && medium.proven);
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
    fn pairs_hinge_where_a_triangle_holds() {
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
    fn dense_candidates_still_yield_a_separated_set() {
        let (outline, sites) = rectangle(250.0, 30.0, 2.5);
        let m = model(1.6);
        let selection = select(&sites, &outline, &m);
        assert!(selection.satisfied(), "{:?}", selection.violations);
        assert!(selection.chosen.len() > 4);
        for (i, &a) in selection.chosen.iter().enumerate() {
            for &b in &selection.chosen[i + 1..] {
                assert!(sites[a].point.distance_to(sites[b].point) >= m.min_separation_mm);
            }
        }
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
