use pcb_elastic::{DMatrix, DVector};
use pcb_ir::geom::mesh::MeshOptions;
use pcb_ir::geom::{Affine2, BBox, ContourSet, Point, Resolution};
use pcb_ir::import::physical::{
    Association, BoardPhysicalDiagnostic, BoardPhysicalMetadata, BoardPhysicalView,
};
use pcb_mechanical::{BoardInstance, Error, Laminate, Panel};

#[test]
fn physical_view_placement_preserves_missing_evidence_and_requires_explicit_material() {
    let resolution = Resolution::default().strict();
    let empty = ContourSet::empty(resolution);
    let board = BoardPhysicalView {
        step: 0,
        substrate: ContourSet::rectangle(BBox::new(Point::ZERO, Point::new(2., 1.)), resolution),
        profile_cutouts: empty.clone(),
        profiles: vec![],
        copper: vec![],
        removal_layers: vec![],
        holes: vec![],
        components: vec![],
        metadata: BoardPhysicalMetadata {
            stackup: Association::Unresolved,
            overall_thickness_mm: None,
            layers: vec![],
            groups: vec![],
            diagnostics: vec![BoardPhysicalDiagnostic::MissingStackup],
        },
        diagnostics: vec![BoardPhysicalDiagnostic::MissingThickness { layer: None }],
        source_diagnostics: vec![],
    };
    let mut instance = BoardInstance {
        board: &board,
        placement: Affine2::translation(Point::new(3., 4.)),
    };
    let panel = Panel::from_physical(std::slice::from_ref(&instance), &empty, &[]).unwrap();
    assert!(panel.substrate.contains_point(Point::new(4., 4.5)));
    assert!(!panel.substrate.contains_point(Point::new(1., 0.5)));
    assert!(
        panel
            .diagnostics
            .iter()
            .any(|d| d.contains("MissingThickness") && d.contains("MissingStackup"))
    );
    let material = Laminate {
        thickness_mm: f64::NAN,
        plane_stress: DMatrix::from_diagonal(&DVector::from_vec(vec![1., 1., 0.5])),
        evidence: None,
    };
    assert!(matches!(
        panel.discretize(
            &material,
            MeshOptions {
                max_area_mm2: 1.,
                min_angle_degrees: 20.,
                max_additional_vertices: 100
            },
            100
        ),
        Err(Error::Input(_))
    ));
    instance.placement.m00 = 2.;
    assert!(matches!(
        Panel::from_physical(&[instance], &empty, &[]),
        Err(Error::Input(_))
    ));
}
