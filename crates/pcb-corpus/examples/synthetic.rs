//! Regenerate analytical inputs, not approved expected outputs.
use pcb_corpus::{Fixture, Overlay, Provenance, VERSION};
use pcb_ir::geom::{
    ContourBuf, ContourSet, FillRule, GeometryAccuracy, PathCmd, Point, Resolution, tol,
};
use serde_json::json;

fn rectangle(x: f64, y: f64, w: f64, h: f64) -> Vec<[f64; 2]> {
    vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}

fn main() -> anyhow::Result<()> {
    let output = std::env::args()
        .nth(1)
        .expect("usage: synthetic OUTPUT_DIRECTORY");
    std::fs::create_dir_all(&output)?;
    let circle = ContourBuf::new(vec![
        PathCmd::move_to(Point::new(10.0, 0.0)),
        PathCmd::arc_to(Point::new(-10.0, 0.0), Point::new(0.0, 0.0), false),
        PathCmd::arc_to(Point::new(10.0, 0.0), Point::new(0.0, 0.0), false),
        PathCmd::close(),
    ]);
    let accuracy = GeometryAccuracy::micrometres(5);
    let circle = ContourSet::from_contours(
        &[circle],
        FillRule::EvenOdd,
        Resolution::new(tol::REGION_MM, accuracy),
    )?;
    let cases = [
        ("curves-hole", circle.rings, vec![rectangle(-2.0,-2.0,4.0,4.0)], vec![], json!({"source_curve": {"kind":"circle", "center_mm":[0,0], "radius_mm":10}, "removal":"4 by 4 mm central square", "source_uncertainty_mm":circle.uncertainty_mm})),
        ("concave-narrow-overhang", vec![vec![[0.0,0.0],[20.0,0.0],[20.0,15.0],[12.0,15.0],[12.0,2.0],[8.0,2.0],[8.0,15.0],[0.0,15.0]], rectangle(2.0,2.0,2.0,2.0)], vec![rectangle(7.0,0.0,6.0,1.9)], vec![Overlay { name:"J1 synthetic body".into(), meaning:"Explicit synthetic body envelope, 2 mm overhang; not inferred from package outline".into(), rings:vec![rectangle(18.0,5.0,4.0,4.0)] }], json!({"clearance_mm":0.1,"clearance_definition":"y=1.9 removal edge to y=2 concavity edge", "hole_mm":[2,2,2,2]})),
        ("complete-removal", vec![rectangle(0.0,0.0,10.0,10.0)], vec![rectangle(0.0,0.0,10.0,10.0)], vec![], json!({"purpose":"geometric rejection, not physical failure"})),
    ];
    for (id, substrate, removal, overlays, evidence) in cases {
        let fixture = Fixture { version: VERSION, id:id.into(), provenance:Provenance {
            repository:"https://github.com/diodeinc/pcb".into(), revision:"corpus-v1".into(), path:"crates/pcb-corpus/examples/synthetic.rs".into(), sha256:None,
            extraction:"Analytical synthetic construction; pcb-ir PathCmd arcs flattened with canonical tolerance".into(),
            limitations:vec!["Synthetic input, not experimental validation or an accepted snapshot".into()],
        }, tolerance_mm:tol::REGION_MM, flatten_mm:accuracy.max_error_mm(), substrate, removal, overlays, evidence };
        std::fs::write(
            format!("{output}/{id}.json"),
            serde_json::to_vec_pretty(&fixture)?,
        )?;
    }
    Ok(())
}
