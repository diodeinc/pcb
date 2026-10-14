//! Times splitting each net's copper by solder-mask openings on a synthetic
//! board, with one `intersection_and_difference` against separate
//! `intersection` and `difference` calls, and checks the results agree.
//! Run: cargo run --release -p pcb-ir --example split_by_openings
use pcb_ir::geom::{ContourSet, FillRule, Resolution};
use std::time::{Duration, Instant};

type Ring = Vec<[f64; 2]>;

const COLUMNS: usize = 120;
const ROWS: usize = 80;
const PITCH: f64 = 1.25;
const NETS: usize = 60;
const POURS: usize = 4;

fn square(x: f64, y: f64, width: f64, height: f64) -> Ring {
    vec![
        [x, y],
        [x + width, y],
        [x + width, y + height],
        [x, y + height],
    ]
}

fn region(rings: Vec<Ring>) -> ContourSet {
    ContourSet::from_rings(rings, FillRule::EvenOdd, Resolution::default()).unwrap()
}

/// The pad at `index` of the board's grid, as `(x, y, width, height)`.
fn pad(index: usize) -> (f64, f64, f64, f64) {
    let (column, row) = (index % COLUMNS, index / COLUMNS);
    let (x, y) = (column as f64 * PITCH + 0.3, row as f64 * PITCH + 0.3);
    (x, y, 0.6 + 0.1 * (row % 3) as f64, 0.4)
}

/// The copper of `net`, which owns every `NETS`th pad. The first `POURS`
/// nets also pour a band of the board, cleared around every other net's
/// pads. With `island`, the pour holds an island in a hole beyond the
/// openings' bounds.
fn net(net: usize, island: bool) -> ContourSet {
    let pads = (net..COLUMNS * ROWS)
        .step_by(NETS)
        .map(|index| {
            let (x, y, width, height) = pad(index);
            square(x, y, width, height)
        })
        .collect::<Vec<_>>();
    let copper = region(pads);
    if net >= POURS {
        return copper;
    }
    let band = ROWS as f64 * PITCH / POURS as f64;
    let (low, high) = (net as f64 * band, (net + 1) as f64 * band);
    let mut pour = vec![square(-0.5, low, COLUMNS as f64 * PITCH + 1.0, band)];
    pour.extend(
        (0..COLUMNS * ROWS)
            .filter(|index| index % NETS != net)
            .map(pad)
            .filter(|&(_, y, _, height)| y - 0.2 > low && y + height + 0.2 < high)
            .map(|(x, y, width, height)| square(x - 0.2, y - 0.2, width + 0.4, height + 0.4)),
    );
    if island {
        pour.push(square(-0.45, low + 1.0, 0.5, 1.0));
        pour.push(square(-0.35, low + 1.25, 0.3, 0.5));
    }
    region(pour).union(&copper).unwrap()
}

fn best_of<T>(runs: usize, mut run: impl FnMut() -> T) -> (Duration, T) {
    let mut best = Duration::MAX;
    let mut last = None;
    for _ in 0..runs {
        let start = Instant::now();
        last = Some(run());
        best = best.min(start.elapsed());
    }
    (best, last.unwrap())
}

fn main() {
    let openings = region(
        (0..COLUMNS * ROWS)
            .map(|index| {
                let (x, y, width, height) = pad(index);
                square(x - 0.05, y - 0.05, width + 0.1, height + 0.1)
            })
            .collect(),
    );
    let pours = (0..POURS)
        .map(|index| net(index, false))
        .collect::<Vec<_>>();
    let islands = (0..POURS).map(|index| net(index, true)).collect::<Vec<_>>();
    let pads = (POURS..NETS)
        .map(|index| net(index, false))
        .collect::<Vec<_>>();
    let vertices = |regions: &[ContourSet]| {
        regions
            .iter()
            .flat_map(ContourSet::rings)
            .map(<[_]>::len)
            .sum::<usize>()
    };
    println!(
        "openings: {} rings, {} vertices",
        openings.rings.len(),
        vertices(std::slice::from_ref(&openings))
    );
    for (name, nets) in [
        ("pour nets", &pours),
        ("pad-only nets", &pads),
        ("pour nets with an island beyond the openings", &islands),
    ] {
        let (separate, expected) = best_of(5, || {
            nets.iter()
                .map(|net| {
                    (
                        net.intersection(&openings).unwrap(),
                        net.difference(&openings).unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        });
        let (combined, actual) = best_of(5, || {
            nets.iter()
                .map(|net| net.intersection_and_difference(&openings).unwrap())
                .collect::<Vec<_>>()
        });
        for ((inside, outside), (expected_inside, expected_outside)) in actual.iter().zip(&expected)
        {
            assert_eq!(inside.rings, expected_inside.rings);
            assert_eq!(outside.rings, expected_outside.rings);
            assert_eq!(inside.uncertainty_mm, expected_inside.uncertainty_mm);
            assert_eq!(outside.uncertainty_mm, expected_outside.uncertainty_mm);
        }
        println!(
            "{} {name} ({} vertices): intersection + difference {separate:.1?}, \
             intersection_and_difference {combined:.1?}, {:.2}x",
            nets.len(),
            vertices(nets),
            separate.as_secs_f64() / combined.as_secs_f64()
        );
    }
}
