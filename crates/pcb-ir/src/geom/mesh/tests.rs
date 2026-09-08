use super::*;
use crate::geom::{Affine2, BBox, FillRule, Mirror, Resolution, shapes};

fn cross(a: Point, b: Point) -> f64 {
    a.x * b.y - a.y * b.x
}

fn options(area: f64) -> MeshOptions {
    MeshOptions {
        max_area_mm2: area,
        min_angle_degrees: 25.0,
        max_additional_vertices: 20_000,
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(x, y), Point::new(x + w, y + h)),
        Resolution::default(),
    )
}

// Independent topological and geometric invariants, not triangulator snapshots.
fn verify(region: &ContourSet, mesh: &AnalysisMesh) {
    assert!((mesh.quality.area_mm2 - region.area()).abs() < 1e-8 * region.area().max(1.0));
    let mut incidence = BTreeMap::<[usize; 2], usize>::new();
    let mut owners = BTreeMap::new();
    for t in &mesh.elements {
        let [a, b, c] = t.vertices.map(|v| mesh.vertices[v]);
        assert!(cross(b - a, c - a) > 0.0);
        assert!(region.contains_point((a + b + c) / 3.0));
        for &v in &t.vertices {
            assert_eq!(*owners.entry(v).or_insert(t.component), t.component);
        }
        for mut edge in [
            [t.vertices[0], t.vertices[1]],
            [t.vertices[1], t.vertices[2]],
            [t.vertices[2], t.vertices[0]],
        ] {
            edge.sort();
            *incidence.entry(edge).or_default() += 1;
        }
    }
    assert_eq!(
        owners.len(),
        mesh.vertices.len(),
        "no orphan analysis nodes"
    );
    let mut intervals = BTreeMap::<(usize, usize), Vec<[f64; 2]>>::new();
    for edge in &mesh.boundary {
        let mut key = edge.vertices;
        key.sort();
        assert_eq!(incidence.remove(&key), Some(1));
        let ring = &region.rings[edge.ring];
        let [x, y] = ring[edge.segment];
        let a = Point::new(x, y);
        let [x, y] = ring[(edge.segment + 1) % ring.len()];
        let b = Point::new(x, y);
        for (v, t) in edge.vertices.into_iter().zip(edge.parameters) {
            assert!(mesh.vertices[v].distance_to(a + (b - a) * t) < 1e-9);
            assert_eq!(owners[&v], edge.component);
        }
        intervals
            .entry((edge.ring, edge.segment))
            .or_default()
            .push(edge.parameters);
    }
    assert!(
        incidence.values().all(|&n| n == 2),
        "conforming interior edges"
    );
    for (ring, r) in region.rings.iter().enumerate() {
        for segment in 0..r.len() {
            let spans = &intervals[&(ring, segment)];
            assert_eq!(spans.first().unwrap()[0], 0.0);
            assert_eq!(spans.last().unwrap()[1], 1.0);
            assert!(spans.iter().all(|s| s[0] < s[1]));
            assert!(spans.windows(2).all(|s| s[0][1] == s[1][0]));
        }
    }
}

#[test]
fn holes_nested_islands_and_thin_features_preserve_topology() {
    let region = rect(0.0, 0.0, 4.0, 3.0)
        .difference(&rect(1.0, 0.1, 2.0, 2.8))
        .unwrap()
        .union(&rect(1.5, 1.0, 1.0, 1.0))
        .unwrap()
        .union(&rect(5.0, 0.0, 2.0, 0.01))
        .unwrap();
    let mesh = AnalysisMesh::new(&region, options(0.05)).unwrap();
    verify(&region, &mesh);
    assert_eq!(mesh.refinement, RefinementStatus::TargetsMet);
    assert_eq!(
        mesh.elements
            .iter()
            .map(|t| t.component)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert!(mesh.attach(Point::new(1.1, 1.0), None, 0.0).is_none());
    assert!(mesh.attach(Point::new(6.0, 0.005), None, 0.0).is_some());
    assert_eq!(mesh, AnalysisMesh::new(&region, options(0.05)).unwrap());
}

#[test]
fn attachments_reproduce_affine_fields_and_expose_sides_and_snapping() {
    let mesh = AnalysisMesh::new(&rect(0.0, 0.0, 2.0, 2.0), options(4.0)).unwrap();
    let p = Point::new(1.0, 1.0);
    let sides: Vec<_> = mesh.attachments(p, None, 0.0).collect();
    assert_eq!(sides.len(), 2);
    assert_eq!(mesh.attach(p, None, 0.0), Some(sides[0]));
    for hit in sides {
        assert_eq!(mesh.attach_to_element(p, hit.element, 0.0), Some(hit));
    }
    for p in [
        Point::ZERO,
        Point::new(0.0, 0.7),
        Point::new(0.3, 1.7),
        Point::new(2.0, 2.0),
    ] {
        let hit = mesh.attach(p, None, 1e-12).unwrap();
        let nodes = mesh.elements[hit.element]
            .vertices
            .map(|v| mesh.vertices[v]);
        let value: f64 = nodes
            .into_iter()
            .zip(hit.weights)
            .map(|(q, w)| (2.0 + 3.0 * q.x - 5.0 * q.y) * w)
            .sum();
        assert!((value - (2.0 + 3.0 * p.x - 5.0 * p.y)).abs() < 1e-12);
        assert!((hit.weights.iter().sum::<f64>() - 1.0).abs() < 1e-12);
    }
    let outside = Point::new(-0.001, 1.0);
    assert!(mesh.attach(outside, None, 0.0).is_none());
    let snapped = mesh.attach(outside, None, 0.002).unwrap();
    assert!((snapped.distance_mm - 0.001).abs() < 1e-12);
    assert!(mesh.attach(p, Some(10), 1.0).is_none());
    assert!(mesh.attach_to_element(p, usize::MAX, 0.0).is_none());
}

#[test]
fn curves_and_transforms_preserve_the_canonical_polygon_not_an_invented_curve() {
    let circle = shapes::circle(4.0).unwrap();
    let hole = shapes::circle(2.0).unwrap();
    for mirror in [Mirror::NONE, Mirror::X] {
        let transform = Affine2::placement(Point::new(20.0, -7.0), 37.0, mirror, 1.7);
        let region = ContourSet::from_contours(
            &[
                circle.clone().transformed(transform),
                hole.clone().transformed(transform),
            ],
            FillRule::EvenOdd,
            Resolution::default(),
        )
        .unwrap();
        let mesh = AnalysisMesh::new(&region, options(0.1)).unwrap();
        verify(&region, &mesh);
        assert_eq!(mesh.approximation.source_curve_error_bound_mm, None);
        assert!(region.uncertainty_mm > 0.0);
        assert_eq!(
            mesh.approximation.region_boundary_uncertainty_mm,
            region.uncertainty_mm
        );
        assert!(
            mesh.attach(transform.transform_point(Point::ZERO), None, 0.0)
                .is_none()
        );
        let hit = mesh
            .attach(transform.transform_point(Point::new(1.5, 0.0)), None, 1e-10)
            .unwrap();
        assert!(hit.weights.iter().all(|w| *w >= 0.0));
        for edge in &mesh.boundary {
            let p = mesh.vertices[edge.vertices[0]].midpoint(mesh.vertices[edge.vertices[1]]);
            assert!(mesh.attach(p, Some(edge.component), 1e-10).is_some());
        }
    }
}

#[test]
fn refinement_converges_for_quadratic_interpolation_on_square_and_annulus() {
    let annulus = ContourSet::from_contours(
        &[shapes::circle(4.0).unwrap(), shapes::circle(2.0).unwrap()],
        FillRule::EvenOdd,
        Resolution::default(),
    )
    .unwrap();
    for region in [rect(-2.0, -2.0, 4.0, 4.0), annulus] {
        let mut errors = Vec::new();
        // Begin below the area scale already imposed by the curved boundary.
        for area in [0.1, 0.025, 0.00625] {
            let mesh = AnalysisMesh::new(&region, options(area)).unwrap();
            verify(&region, &mesh);
            assert_eq!(mesh.refinement, RefinementStatus::TargetsMet);
            let mut error = 0.0;
            for t in &mesh.elements {
                let [a, b, c] = t.vertices.map(|v| mesh.vertices[v]);
                let p = (a + b + c) / 3.0;
                let interpolated = (dot(a, a) + dot(b, b) + dot(c, c)) / 3.0;
                error += cross(b - a, c - a) / 2.0 * (interpolated - dot(p, p)).powi(2);
            }
            errors.push((error / region.area()).sqrt());
        }
        assert!(
            errors.windows(2).all(|e| e[1] < 0.5 * e[0]),
            "quadratic interpolation error: {errors:?}"
        );
    }
}

#[test]
fn zero_budget_classifies_measured_quality_across_components() {
    let region = rect(0.0, 0.0, 2.0, 2.0)
        .union(&rect(3.0, 0.0, 1.0, 1.0))
        .unwrap();
    let mut initial_only = options(2.0);
    initial_only.max_additional_vertices = 0;
    let mesh = AnalysisMesh::new(&region, initial_only).unwrap();
    verify(&region, &mesh);
    assert!(mesh.quality.max_area_mm2 <= initial_only.max_area_mm2);
    assert!(mesh.quality.min_angle_degrees >= initial_only.min_angle_degrees);
    assert_eq!(mesh.refinement, RefinementStatus::TargetsMet);

    initial_only.max_area_mm2 = 0.25;
    let mesh = AnalysisMesh::new(&region, initial_only).unwrap();
    verify(&region, &mesh);
    assert!(mesh.quality.max_area_mm2 > initial_only.max_area_mm2);
    assert_eq!(mesh.refinement, RefinementStatus::VertexBudgetExhausted);
}

#[test]
fn exhaustion_is_a_valid_partial_mesh_and_quality_limits_are_explicit() {
    let region = rect(0.0, 0.0, 4.0, 4.0)
        .difference(&rect(1.0, 1.0, 2.0, 2.0))
        .unwrap();
    let mut limited = options(0.001);
    limited.max_additional_vertices = 0;
    let mesh = AnalysisMesh::new(&region, limited).unwrap();
    assert_eq!(mesh.refinement, RefinementStatus::VertexBudgetExhausted);
    verify(&region, &mesh);
    let acute = ContourSet::from_rings(
        vec![vec![[0.0, 0.0], [10.0, 0.0], [0.0, 0.1]]],
        FillRule::NonZero,
        Resolution::default(),
    )
    .unwrap();
    let mesh = AnalysisMesh::new(&acute, options(1.0)).unwrap();
    verify(&acute, &mesh);
    assert_eq!(mesh.refinement, RefinementStatus::QualityLimited);
    assert!(mesh.quality.min_angle_degrees < 1.0);
}

#[test]
fn concave_neck_and_touching_components_keep_separate_nodes() {
    let concave = rect(0.0, 0.0, 4.0, 3.0)
        .difference(&rect(1.0, 0.1, 2.0, 3.0))
        .unwrap();
    let mesh = AnalysisMesh::new(&concave, options(0.03)).unwrap();
    verify(&concave, &mesh);
    assert!(mesh.attach(Point::new(2.0, 2.0), None, 0.0).is_none());
    // Supply exactly touching canonical rings, without a boolean operation's
    // coordinate quantization turning contact into a tiny gap or overlap.
    let region = ContourSet::from_regularized(
        rect(0.0, 0.0, 1.0, 1.0)
            .rings
            .into_iter()
            .chain(rect(1.0, 1.0, 1.0, 1.0).rings)
            .collect(),
        Resolution::default(),
        0.0,
    );
    let mesh = AnalysisMesh::new(&region, options(0.03)).unwrap();
    verify(&region, &mesh);
    let hits: Vec<_> = mesh.attachments(Point::new(1.0, 1.0), None, 0.0).collect();
    assert_eq!(
        hits.iter()
            .map(|h| mesh.elements[h.element].component)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
}

#[test]
fn empty_and_invalid_queries_and_options() {
    let empty = AnalysisMesh::new(&ContourSet::empty(Resolution::default()), options(1.0)).unwrap();
    assert!(empty.elements.is_empty());
    assert!(empty.attach(Point::ZERO, None, 0.0).is_none());
    for area in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert_eq!(
            AnalysisMesh::new(&rect(0.0, 0.0, 1.0, 1.0), options(area)),
            Err(MeshError::InvalidOptions)
        );
    }
    let mesh = AnalysisMesh::new(&rect(0.0, 0.0, 1.0, 1.0), options(1.0)).unwrap();
    assert!(mesh.attach(Point::new(f64::NAN, 0.0), None, 1.0).is_none());
    assert!(mesh.attach(Point::ZERO, None, -1.0).is_none());
    let mut overflow = options(1.0);
    overflow.max_additional_vertices = usize::MAX;
    assert_eq!(
        AnalysisMesh::new(&rect(0.0, 0.0, 1.0, 1.0), overflow),
        Err(MeshError::InvalidOptions)
    );
}

#[test]
fn exact_oblique_boundary_attachment_survives_cancellation() {
    // Exactly collinear binary inputs; ordinary cross products round negative.
    // Power-of-two scaling preserves that relation at different physical sizes.
    for scale in [2.0_f64.powi(-42), 2.0_f64.powi(-40), 2.0_f64.powi(-38)] {
        let a = Point::new(3109757648896.0, 20809216.0) * scale;
        let b = Point::new(-29.376953125, -825439027200.0) * scale;
        let p = Point::new(388719706086.29517, -722256547648.0) * scale;
        assert!(cross(a - p, b - p) < 0.0);
        let mut mesh = AnalysisMesh::new(&rect(0.0, 0.0, 1.0, 1.0), options(1.0)).unwrap();
        mesh.vertices = vec![
            a,
            b,
            Point::new(4398046511104.0, 0.0) * scale,
            Point::new(-1099511627776.0, 0.0) * scale,
        ];
        mesh.elements = vec![
            MeshElement {
                vertices: [0, 1, 2],
                component: 0,
            },
            MeshElement {
                vertices: [1, 0, 3],
                component: 0,
            },
        ];
        let hits: Vec<_> = mesh.attachments(p, None, 0.0).collect();
        assert_eq!(hits.len(), 2, "both physical sides must be available");
        for hit in &hits {
            assert_eq!(hit.distance_mm, 0.0);
            let expected = if hit.element == 0 {
                [0.125, 0.875]
            } else {
                [0.875, 0.125]
            };
            for (actual, expected) in hit.weights.iter().zip(expected) {
                assert!((actual - expected).abs() <= 2.0 * f64::EPSILON);
            }
            assert_eq!(hit.weights[2], 0.0);
            assert!(
                hit.weights
                    .iter()
                    .all(|w| w.is_finite() && (0.0..=1.0).contains(w))
            );
            assert!((hit.weights.iter().sum::<f64>() - 1.0).abs() <= 16.0 * f64::EPSILON);
        }
        assert_eq!(mesh.attach(p, None, 0.0).unwrap().element, 0);
        // One representable step outside must not become implicit snapping.
        let outside = Point::new(p.x, p.y.next_up());
        assert!(mesh.attach_to_element(outside, 0, 0.0).is_none());
        assert!(mesh.attach_to_element(outside, 0, 1e-12).is_some());
    }
}

#[test]
fn restored_regions_must_have_valid_resolution_and_accuracy_history() {
    let resolution = Resolution::default();
    let budget = resolution.accuracy.max_error_mm();
    for rings in [vec![], rect(0.0, 0.0, 1.0, 1.0).rings] {
        for uncertainty in [budget.next_up(), f64::INFINITY, f64::NAN, -1.0] {
            let region = ContourSet::from_regularized(rings.clone(), resolution, uncertainty);
            let error = AnalysisMesh::new(&region, options(1.0)).unwrap_err();
            assert!(matches!(error, MeshError::Accuracy(_)));
            assert!(error.to_string().contains("accuracy budget"));
        }
        for tolerance in [-0.001, f64::NAN, f64::INFINITY] {
            let region = ContourSet::from_regularized(
                rings.clone(),
                resolution.with_tolerance(tolerance),
                0.0,
            );
            assert_eq!(
                AnalysisMesh::new(&region, options(1.0)),
                Err(MeshError::InvalidRegion)
            );
        }
        // Triangulating the already-prepared polygon spends no new boundary budget.
        for uncertainty in [0.0, budget] {
            let region = ContourSet::from_regularized(rings.clone(), resolution, uncertainty);
            let mesh = AnalysisMesh::new(&region, options(1.0)).unwrap();
            assert_eq!(
                mesh.approximation.region_boundary_uncertainty_mm,
                uncertainty
            );
        }
    }
}

#[test]
fn constructor_preserves_cancellation_prone_thin_triangles() {
    let points = [
        [-3.656357558875988, 3.4743373693723267],
        [2.6377461897661405, -2.449309742605783],
        [-0.5380377200017632, 0.5395547465475277],
    ];
    // Exact rational determinant of the represented binary coordinates, rounded
    // to f64. Ordinary translated determinants give either zero or 3.55e-15.
    let expected_area = 6.649046188380301e-16 / 2.0;
    for scale in [0.25, 1.0, 4.0] {
        // Start at the vertex for which upstream polygon area remains positive.
        let ring = (0..3)
            .map(|i| points[(i + 1) % 3].map(|x| x * scale))
            .collect();
        let region = ContourSet::from_regularized(vec![ring], Resolution::default().strict(), 0.0);
        assert_eq!(region.rings.len(), 1);
        let mesh = AnalysisMesh::new(
            &region,
            MeshOptions {
                max_area_mm2: 1.0,
                min_angle_degrees: 0.0,
                max_additional_vertices: 0,
            },
        )
        .unwrap();
        assert_eq!(mesh.elements.len(), 1);
        assert_eq!(mesh.boundary.len(), 3);
        assert!((mesh.quality.area_mm2 / (expected_area * scale * scale) - 1.0).abs() < 1e-14);
        assert!(mesh.quality.min_angle_degrees > 0.0);
        assert_eq!(mesh.refinement, RefinementStatus::TargetsMet);
    }
}
