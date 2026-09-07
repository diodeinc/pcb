use pcb_elastic::{Analysis, DMatrix, DVector, Model, Status, Tolerances, elements};
use pcb_ir::geom::attachment::{BoundaryQuery, QueryTolerance};
use pcb_ir::geom::mesh::MeshOptions;
use pcb_ir::geom::mouse_bite::{self, TabGeometry};
use pcb_ir::geom::{BBox, ContourSet, Point, Resolution};
use pcb_mechanical::*;

fn rect(x: f64, y: f64, w: f64, h: f64) -> ContourSet {
    ContourSet::rectangle(
        BBox::new(Point::new(x, y), Point::new(x + w, y + h)),
        Resolution::default().strict(),
    )
}
fn material(t: f64) -> Laminate {
    // Synthetic nu=0, E=12, not an FR4 recommendation. D=1 at t=1.
    Laminate {
        thickness_mm: t,
        plane_stress: DMatrix::from_diagonal(&DVector::from_vec(vec![12., 12., 6.])),
        evidence: None,
    }
}
fn options(area: f64) -> MeshOptions {
    MeshOptions {
        max_area_mm2: area,
        min_angle_degrees: 20.,
        max_additional_vertices: 10000,
    }
}
fn panel(region: &ContourSet) -> Panel {
    Panel::from_regions(&[], region, &[]).unwrap()
}
fn case(loads: Vec<Load>) -> ProcessCase {
    ProcessCase {
        name: "synthetic verification only".into(),
        loads,
        rails: vec![],
        tooling: vec![],
        evidence: None,
    }
}
fn rail(d: &Discretization, x: f64, restraint: Restraint) -> RailFixture {
    RailFixture {
        boundary_edges: d
            .mesh()
            .boundary
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.vertices
                    .iter()
                    .all(|&v| (d.mesh().vertices[v].x - x).abs() < 1e-9)
            })
            .map(|(i, _)| i)
            .collect(),
        restraint,
    }
}
fn solve(p: &Problem) -> Analysis {
    Model::new(
        p.scales.clone(),
        &p.contributions,
        Tolerances {
            rank_relative: 1e-12,
            rank_absolute: 0.,
            residual_relative: 1e-10,
            residual_absolute: 1e-10,
        },
    )
    .unwrap()
    .evaluate(&[], &p.forces, &p.prescribed)
    .unwrap()
}
fn close(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() < tol, "{a} != {b} (tolerance {tol})");
}

#[test]
fn cdt_cantilever_refines_to_beam_and_scales_with_thickness() {
    // Cylindrical bending is exact at nu=0: compliance p² b L⁵/(20D).
    let domain = panel(&rect(0., 0., 4., 1.));
    let exact = 4_f64.powi(5) / 20.;
    let mut errors = Vec::new();
    for area in [0.5, 0.125, 0.03125] {
        let d = domain
            .discretize(&material(1.), options(area), 2000)
            .unwrap();
        let mut c = case(vec![Load::Pressure(1.)]);
        c.rails.push(rail(&d, 0., Restraint::Clamped));
        let r = solve(&d.problem(&c).unwrap());
        assert_eq!(r.status, Status::Stable);
        let error = (r.compliance / exact - 1.).abs();
        eprintln!(
            "cantilever area={area}, DOFs={}, compliance={}, exact={exact}, relative error={error}",
            r.displacement.len(),
            r.compliance
        );
        errors.push(error);
        close(
            r.reactions.iter().take(d.mesh().vertices.len()).sum(),
            -4.,
            1e-6,
        );
    }
    assert!(errors[1] < errors[0] && errors[2] < errors[1]);
    assert!(errors[2] < 0.02);
    let mut values = Vec::new();
    for t in [0.5, 1., 2.] {
        let d = domain.discretize(&material(t), options(0.5), 1000).unwrap();
        let mut c = case(vec![Load::Pressure(1.)]);
        c.rails.push(rail(&d, 0., Restraint::Clamped));
        let r = solve(&d.problem(&c).unwrap());
        assert_eq!(r.status, Status::Stable);
        values.push(r.compliance * t.powi(3));
    }
    close(values[0], values[1], 1e-7);
    close(values[1], values[2], 1e-7);
    // Independently exercise the shared beam API with exact consistent UDL.
    let l: f64 = 4.;
    let k = elements::beam(l, 1.).unwrap();
    let f = DVector::from_vec(vec![l / 2., l * l / 12., l / 2., -l * l / 12.]);
    let u = k
        .view((2, 2), (2, 2))
        .into_owned()
        .lu()
        .solve(&f.rows(2, 2))
        .unwrap();
    close(u[0], l.powi(4) / 8., 1e-12);
}

#[test]
fn canonical_normals_p2_virtual_work_and_component_isolation() {
    let domain = rect(0., 0., 2., 1.).union(&rect(3., 0., 1., 1.)).unwrap();
    let d = panel(&domain)
        .discretize(&material(1.), options(0.5), 1000)
        .unwrap();
    let r = solve(&d.problem(&case(vec![])).unwrap());
    assert_eq!(r.status, Status::SingularCompatible);
    assert_eq!(r.unsupported_modes.len(), 6);
    let p = &d.mesh().vertices;
    let t = &d.mesh().elements[0];
    let point = t.vertices.iter().fold(Point::ZERO, |s, &v| s + p[v]) / 3.;
    let site = Site { element: 0, point };
    let (dofs, rows) = d.observation(site).unwrap();
    let quadratic = |p: Point| p.x * p.x + 2. * p.y * p.y + 3. * p.x * p.y;
    let gradient = |p: Point| [2. * p.x + 3. * p.y, 4. * p.y + 3. * p.x];
    let mut local = DVector::zeros(6);
    for i in 0..3 {
        local[i] = quadratic(p[t.vertices[i]]);
        let a = t.vertices[i].min(t.vertices[(i + 1) % 3]);
        let b = t.vertices[i].max(t.vertices[(i + 1) % 3]);
        let delta = p[b] - p[a];
        let g = gradient((p[a] + p[b]) / 2.);
        local[3 + i] = (g[0] * delta.y - g[1] * delta.x) / delta.x.hypot(delta.y);
    }
    let observed = &rows * &local;
    close(observed[0], quadratic(point), 1e-12);
    close(observed[1], gradient(point)[0], 1e-12);
    close(observed[2], gradient(point)[1], 1e-12);
    let loaded = d
        .problem(&case(vec![Load::Point {
            site,
            force_z: 2.,
            moment_xy: [3., 4.],
        }]))
        .unwrap();
    let work: f64 = dofs
        .iter()
        .enumerate()
        .map(|(j, &i)| loaded.forces[i] * local[j])
        .sum();
    close(
        work,
        2. * quadratic(point) + 3. * gradient(point)[1] - 4. * gradient(point)[0],
        1e-11,
    );
    assert!(loaded.diagnostics.iter().any(|s| s.contains("UNVALIDATED")));
    assert!(matches!(
        panel(&domain).discretize(&material(1.), options(0.5), 1),
        Err(Error::ResourceLimit { .. })
    ));
    let mut limited = options(0.00001);
    limited.max_additional_vertices = 0;
    assert!(matches!(
        panel(&domain).discretize(&material(1.), limited, 1000),
        Err(Error::Refinement(_))
    ));
}

fn tab(board: &ContourSet, support: &ContourSet, stock: &ContourSet, x: f64) -> TabGeometry {
    let tolerance = QueryTolerance {
        boundary_mm: 0.,
        numerical_mm: 1e-7,
    };
    let q = BoundaryQuery::new(board, tolerance).unwrap();
    let boundary = q.boundaries().next().unwrap();
    let station_mm = q
        .project(boundary, Point::new(x, 0.))
        .unwrap()
        .site
        .station_mm;
    mouse_bite::build(mouse_bite::Attachment {
        stock,
        board,
        support,
        boundary,
        station_mm,
        support_anchor: Point::new(x, 4.),
        board_witness: Point::new(0., -2.),
        tolerance,
    })
    .unwrap()
}

#[test]
fn full_stock_tab_decomposition_keeps_both_necks_and_all_holes() {
    let board = rect(-6., -4., 12., 4.);
    let frame = rect(-6., 3., 12., 2.);
    let stock = rect(-6., -4., 12., 9.);
    let tabs = vec![
        tab(&board, &frame, &stock, -3.),
        tab(&board, &frame, &stock, 3.),
    ];
    let p = Panel::from_regions(std::slice::from_ref(&board), &frame, &tabs).unwrap();
    let expected = tabs[0]
        .retained_substrate
        .union(&tabs[1].retained_substrate)
        .unwrap()
        .difference(&tabs[0].perforations.union(&tabs[1].perforations).unwrap())
        .unwrap();
    close(p.substrate.area(), expected.area(), 1e-6);
    assert_eq!(p.substrate.connected_components().len(), 1);
    for t in &tabs {
        assert!(p.substrate.intersection(&t.perforations).unwrap().area() < 1e-8);
    }
    assert!(
        p.substrate.contains_point(Point::new(-3., 1.5))
            && p.substrate.contains_point(Point::new(3., 1.5))
    );
    assert!(!p.substrate.contains_point(Point::new(0., 1.5)));
}

#[test]
fn perforated_tab_response_and_refinement() {
    let board = rect(-3., -4., 6., 4.);
    let frame = rect(-3., 3., 6., 2.);
    let stock = rect(-3., -4., 6., 9.);
    let tab = tab(&board, &frame, &stock, 0.);
    let perforated = Panel::from_regions(
        std::slice::from_ref(&board),
        &frame,
        std::slice::from_ref(&tab),
    )
    .unwrap();
    let unperforated = panel(
        &board
            .union(&frame)
            .unwrap()
            .union(&tab.attachment_footprint)
            .unwrap(),
    );
    let mut results = Vec::new();
    for (name, domain, area) in [
        ("drilled coarse", &perforated, 1.),
        ("drilled fine", &perforated, 0.25),
        ("undrilled", &unperforated, 0.25),
    ] {
        let d = domain
            .discretize(&material(1.), options(area), 2500)
            .unwrap();
        eprintln!(
            "{name}: {} vertices, {} triangles, quality {:?}",
            d.mesh().vertices.len(),
            d.mesh().elements.len(),
            d.mesh().quality
        );
        let mut c = case(vec![]);
        c.rails.push(RailFixture {
            boundary_edges: d
                .mesh()
                .boundary
                .iter()
                .enumerate()
                .filter(|(_, b)| {
                    b.vertices
                        .iter()
                        .all(|&v| (d.mesh().vertices[v].y - 5.).abs() < 1e-9)
                })
                .map(|(i, _)| i)
                .collect(),
            restraint: Restraint::Clamped,
        });
        // Same physical point and resultant on every mesh. No changing
        // rasterized contact area is hidden in the refinement comparison.
        let point = Point::new(0., -3.);
        let element = d
            .mesh()
            .attachments(point, None, 0.)
            .next()
            .unwrap()
            .element;
        c.loads.push(Load::Point {
            site: Site { element, point },
            force_z: 1.,
            moment_xy: [0., 0.],
        });
        let p = d.problem(&c).unwrap();
        let r = solve(&p);
        assert_eq!(r.status, Status::Stable);
        eprintln!(
            "{name}: DOFs={}, unit resultant compliance={}",
            r.displacement.len(),
            r.compliance
        );
        results.push(r.compliance);
    }
    assert!((results[0] / results[1] - 1.).abs() < 0.15);
    assert!(results[1] > results[2]);
}

#[test]
fn whole_panel_rail_flexibility_and_tooling_not_independent_cell_clamps() {
    // Two wide board pads on a shared flexible retained rail, narrow continuum
    // bridges. Synthetic unperforated tabs isolate global coupling from fracture.
    let rail_region = rect(0., 0., 12., 1.);
    let cells = rect(2., -4., 3., 3.).union(&rect(7., -4., 3., 3.)).unwrap();
    let region = rail_region
        .union(&cells)
        .unwrap()
        .union(&rect(3., -1., 1., 1.))
        .unwrap()
        .union(&rect(8., -1., 1., 1.))
        .unwrap();
    let d = panel(&region)
        .discretize(&material(1.), options(0.4), 2000)
        .unwrap();
    let point = Point::new(3.5, -3.);
    let a = d.mesh().attachments(point, None, 0.).next().unwrap();
    let site = Site {
        element: a.element,
        point,
    };
    let mut c = case(vec![Load::Point {
        site,
        force_z: 1.,
        moment_xy: [0., 0.],
    }]);
    c.rails = vec![
        rail(&d, 0., Restraint::Clamped),
        rail(&d, 12., Restraint::Clamped),
    ];
    let whole = solve(&d.problem(&c).unwrap());
    assert_eq!(whole.status, Status::Stable);
    c.rails = vec![
        rail(&d, 0., Restraint::SimplySupported),
        rail(&d, 12., Restraint::SimplySupported),
    ];
    let simple = solve(&d.problem(&c).unwrap());
    assert_eq!(simple.status, Status::Stable);
    assert!(simple.compliance > whole.compliance);
    c.tooling.push(ToolingSpring {
        site,
        stiffness_n_mm: 1.,
    });
    let tool = solve(&d.problem(&c).unwrap());
    assert_eq!(tool.status, Status::Stable);
    assert!(tool.compliance < simple.compliance);
    // Explicit comparison surrogate only: fix every rail boundary. This is
    // NOT the default and understates global rail bending under this load.
    c.tooling.clear();
    c.rails = vec![RailFixture {
        boundary_edges: d
            .mesh()
            .boundary
            .iter()
            .enumerate()
            .filter(|(_, b)| b.vertices.iter().all(|&v| d.mesh().vertices[v].y >= 0.))
            .map(|(i, _)| i)
            .collect(),
        restraint: Restraint::Clamped,
    }];
    let local = solve(&d.problem(&c).unwrap());
    assert_eq!(local.status, Status::Stable);
    eprintln!(
        "whole-panel compliance: end-clamped={}, end-simple={}, tooling={}, locally fixed rail={}, whole/local={}",
        whole.compliance,
        simple.compliance,
        tool.compliance,
        local.compliance,
        whole.compliance / local.compliance
    );
    assert!(whole.compliance > local.compliance * 1.5);
}
