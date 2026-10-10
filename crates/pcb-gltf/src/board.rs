//! The board side of a [`Scene`](pcb_step::scene::Scene) as triangles:
//! prisms become two caps and their walls, round holes are revolved from
//! their profiles, and flat faces become one cap each. Curved walls get
//! the exact normal of the arc or cylinder they lie on; everything else is
//! flat shaded.
//!
//! Copper is drawn flat, as KiCad's VRML export draws it: caps without
//! walls. Caps resting on the board body and copper inside it are left
//! out.

use std::f64::consts::TAU;

use glam::{DVec2, DVec3};
use i_triangle::float::triangulator::Triangulator;
use i_triangle::i_overlay::core::fill_rule::FillRule;
use i_triangle::i_overlay::core::solver::Solver;
use i_triangle::int::validation::Validation;
use pcb_step::scene::{Layer, LayerKind, Prism, RoundHole, Shape, Vertex};
use rayon::prelude::*;

use crate::mesh::Primitive;

/// Chord error for arcs and circles, in millimetres.
const MAX_ERROR: f64 = 0.01;

/// Chord error for the board body's outline, cutouts and drills: KiCad's
/// VRML tolerance, so large round outlines do not look faceted.
const OUTLINE_ERROR: f64 = 0.005;

/// Tolerance for a copper face to count as resting on the body.
const TOUCHING: f64 = 1e-6;

/// A copper prism, drawn flat: the board body's bottom and top, when it
/// is exported, hide the caps resting on it and the copper inside it.
#[derive(Clone, Copy)]
struct Copper {
    body: Option<(f64, f64)>,
}

/// One optimized primitive per layer, in order.
pub(crate) fn mesh(layers: &[Layer]) -> Vec<Primitive> {
    let body = layers
        .iter()
        .find_map(|layer| match (&layer.kind, &layer.shape) {
            (LayerKind::Body, Shape::Solids(prisms)) => prisms.first().map(|p| (p.z0, p.z1)),
            _ => None,
        });
    layers
        .par_iter()
        .map(|layer| {
            let mut primitive = self::layer(layer, Copper { body });
            primitive.optimize();
            primitive
        })
        .collect()
}

fn layer(layer: &Layer, copper: Copper) -> Primitive {
    let copper = matches!(
        layer.kind,
        LayerKind::Copper | LayerKind::Pads | LayerKind::Vias
    )
    .then_some(copper);
    let parts: Vec<Primitive> = match &layer.shape {
        Shape::Solids(prisms) => {
            let error = if layer.kind == LayerKind::Body {
                OUTLINE_ERROR
            } else {
                MAX_ERROR
            };
            prisms.par_iter().map(|p| prism(p, copper, error)).collect()
        }
        Shape::Faces { z, up, faces } => faces
            .par_iter()
            .map(|face| {
                let rings: Vec<Vec<DVec2>> = std::iter::once(&face.outer)
                    .chain(&face.holes)
                    .map(|l| points(&l.polyline(MAX_ERROR)))
                    .collect();
                let mut out = Primitive::default();
                cap(&mut out, &rings, *z, *up);
                out
            })
            .collect(),
    };
    let mut out = Primitive::default();
    for part in parts {
        out.append(part);
    }
    out
}

/// A prism's faces, with curves chorded within `error` millimetres; only
/// its visible caps when it is copper.
fn prism(prism: &Prism, copper: Option<Copper>, error: f64) -> Primitive {
    let Prism { z0, z1, solid } = prism;
    let (z0, z1) = (*z0, *z1);
    // Which faces the body hides: all of copper inside it, and a cap
    // resting on it.
    let (top_hidden, bottom_hidden) = match copper.and_then(|c| c.body) {
        Some((b0, b1)) if z0 >= b0 - TOUCHING && z1 <= b1 + TOUCHING => {
            return Primitive::default();
        }
        Some((b0, b1)) => (
            (b0 - TOUCHING..b1 - TOUCHING).contains(&z1),
            (b0 + TOUCHING..b1 + TOUCHING).contains(&z0),
        ),
        None => (false, false),
    };
    let mut out = Primitive::default();
    let outer = solid.outer.polyline(error);
    let holes: Vec<Vec<Vertex>> = solid.holes.iter().map(|h| h.polyline(error)).collect();
    let mut top: Vec<Vec<DVec2>> = std::iter::once(&outer)
        .chain(&holes)
        .map(|r| points(r))
        .collect();
    let mut bottom = top.clone();
    for hole in &solid.round {
        let n = segments(hole.max_radius(), error);
        let (z_top, r_top) = hole.profile[0];
        let (z_bottom, r_bottom) = hole.profile[hole.profile.len() - 1];
        if z_top >= z1 - 1e-9 && r_top > 0.0 {
            top.push(circle(hole.center, r_top, n));
        }
        if z_bottom <= z0 + 1e-9 && r_bottom > 0.0 {
            bottom.push(circle(hole.center, r_bottom, n));
        }
        if copper.is_none() {
            round_hole(&mut out, hole, n);
        }
    }
    if !top_hidden {
        cap(&mut out, &top, z1, true);
    }
    if !bottom_hidden {
        cap(&mut out, &bottom, z0, false);
    }
    if copper.is_none() {
        walls(&mut out, &outer, true, z0, z1);
        for hole in &holes {
            walls(&mut out, hole, false, z0, z1);
        }
    }
    out
}

fn points(ring: &[Vertex]) -> Vec<DVec2> {
    ring.iter().map(|v| v.point).collect()
}

/// Chords for a whole circle of radius `r` within `error`.
fn segments(r: f64, error: f64) -> usize {
    let step = 2.0 * (1.0 - error / r).clamp(-1.0, 1.0).acos();
    ((TAU / step.max(1e-3)).ceil() as usize).max(8)
}

fn circle(c: DVec2, r: f64, n: usize) -> Vec<DVec2> {
    (0..n)
        .map(|k| c + DVec2::from_angle(TAU * k as f64 / n as f64) * r)
        .collect()
}

/// A flat region at `z`, facing up or down. Rings may wind either way:
/// they are filled even-odd.
fn cap(out: &mut Primitive, rings: &[Vec<DVec2>], z: f64, up: bool) {
    let contours: Vec<Vec<[f64; 2]>> = rings
        .iter()
        .filter(|r| r.len() >= 3)
        .map(|r| r.iter().map(|p| p.to_array()).collect())
        .collect();
    if contours.is_empty() {
        return;
    }
    let count = contours.iter().map(Vec::len).sum();
    let mut triangulator = Triangulator::<u32, i32>::new(
        count,
        Validation::with_fill_rule(FillRule::EvenOdd),
        Solver::default(),
    );
    let triangulation = triangulator.triangulate(&contours);
    let normal = if up { DVec3::Z } else { DVec3::NEG_Z };
    let base = out.positions.len() as u32;
    for [x, y] in triangulation.points {
        out.vertex(DVec3::new(x, y, z), normal);
    }
    for t in triangulation.indices.as_chunks::<3>().0 {
        // Triangles come counter-clockwise, facing up.
        if up {
            out.indices.extend([base + t[0], base + t[1], base + t[2]]);
        } else {
            out.indices.extend([base + t[0], base + t[2], base + t[1]]);
        }
    }
}

/// The side walls of a ring between `z0` and `z1`, facing away from the
/// material: out of an outer ring, into a hole.
fn walls(out: &mut Primitive, ring: &[Vertex], outer: bool, z0: f64, z1: f64) {
    let area: f64 = (0..ring.len())
        .map(|i| ring[i].point.perp_dot(ring[(i + 1) % ring.len()].point))
        .sum();
    // The right of each chord faces out of a counter-clockwise outer ring
    // and into a clockwise hole.
    let side = if (area > 0.0) == outer { 1.0 } else { -1.0 };
    for (i, a) in ring.iter().enumerate() {
        let b = ring[(i + 1) % ring.len()];
        let d = b.point - a.point;
        if d.length_squared() < 1e-18 {
            continue;
        }
        let flat = (DVec2::new(d.y, -d.x).normalize() * side).extend(0.0);
        let (na, nb) = match a.center {
            Some(c) => {
                let sign = (a.point - c).dot(flat.truncate()).signum();
                (
                    ((a.point - c).normalize() * sign).extend(0.0),
                    ((b.point - c).normalize() * sign).extend(0.0),
                )
            }
            None => (flat, flat),
        };
        out.quad(
            [
                (a.point.extend(z0), na),
                (b.point.extend(z0), nb),
                (b.point.extend(z1), nb),
                (a.point.extend(z1), na),
            ],
            flat,
        );
    }
}

/// The walls, shoulders and floor of a round hole, revolved from its
/// profile with `n` chords and facing into the void.
fn round_hole(out: &mut Primitive, hole: &RoundHole, n: usize) {
    for pair in hole.profile.windows(2) {
        let [(z0, r0), (z1, r1)] = [pair[0], pair[1]];
        if (z0 - z1).abs() < 1e-9 {
            // A shoulder between two radii faces up when the hole narrows
            // going down; a floor (radius zero) faces up too.
            let (wide, narrow, up) = if r0 > r1 {
                (r0, r1, true)
            } else {
                (r1, r0, false)
            };
            if wide <= 0.0 {
                continue;
            }
            let mut rings = vec![circle(hole.center, wide, n)];
            if narrow > 0.0 {
                rings.push(circle(hole.center, narrow, n));
            }
            cap(out, &rings, z0, up);
            continue;
        }
        // The profile runs top down; the wall leans towards the axis.
        let (dz, dr) = (z0 - z1, r0 - r1);
        for k in 0..n {
            let [ua, ub] = [k, k + 1].map(|j| DVec2::from_angle(TAU * j as f64 / n as f64));
            let normal = |u: DVec2| (u * -dz).extend(dr).normalize();
            let at = |u: DVec2, r: f64, z: f64| (hole.center + u * r).extend(z);
            let facing = normal(DVec2::from_angle(TAU * (k as f64 + 0.5) / n as f64));
            out.quad(
                [
                    (at(ua, r1, z1), normal(ua)),
                    (at(ub, r1, z1), normal(ub)),
                    (at(ub, r0, z0), normal(ub)),
                    (at(ua, r0, z0), normal(ua)),
                ],
                facing,
            );
        }
    }
}
