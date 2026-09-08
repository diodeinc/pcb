//! Boundary-following clearance review over genuine corpus source exports.
//! See outline_review/README.md for evidence and geometry contracts.

use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};
use pcb_ir::{
    dialects::{assembly, ipc::LayoutStepKind},
    geom::{
        Affine2, ContourSet, FillRule, GeometryAccuracy, Point, Resolution, Ring,
        attachment::{
            QueryTolerance,
            outline::{OutlineFootprint, OutlineObstacle, OutlineState, eligible_outline},
        },
        region::rings_to_contours,
        shapes,
    },
    import::ipc2581::import_design,
    render::svg_path_data,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct Envelope {
    side: String,
    rings: Vec<Ring>,
    circle: Option<Circle>,
}

#[derive(Deserialize)]
struct Circle {
    center: [f64; 2],
    radius_mm: f64,
}

#[derive(Deserialize)]
struct Component {
    id: String,
    dnp: bool,
    physical_role: String,
    envelopes: Vec<Envelope>,
    issues: Vec<String>,
}

#[derive(Deserialize)]
struct Courtyards {
    version: u32,
    name: String,
    xml_sha256: String,
    components: Vec<Component>,
}

fn path(region: &ContourSet) -> String {
    svg_path_data(&rings_to_contours(region.rings.clone()))
}

fn review(
    xml_path: &Path,
    source: Value,
    footprints: &[OutlineFootprint],
    resolution: Resolution,
) -> Result<Value> {
    let courtyards: Courtyards = serde_json::from_value(source.clone())?;
    ensure!(
        courtyards.version == 2,
        "unsupported courtyard extraction version"
    );
    let xml = fs::read(xml_path)?;
    let xml_hash = Sha256::digest(&xml)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ensure!(
        xml_hash == courtyards.xml_sha256,
        "XML hash disagrees with courtyard provenance"
    );
    let ipc = ipc2581::Ipc2581::parse(std::str::from_utf8(&xml)?)?;
    let design = import_design(&ipc, resolution)?;
    let assembly = design.assembly_document(assembly::Scope::Board)?;
    let boards = assembly
        .steps
        .iter()
        .filter(|step| step.kind == LayoutStepKind::Board)
        .collect::<Vec<_>>();
    ensure!(
        boards.len() == 1,
        "review requires exactly one canonical board"
    );
    let mut substrate = ContourSet::empty(resolution);
    for profile in &boards[0].profiles {
        let outer =
            ContourSet::from_filled_contours(std::slice::from_ref(&profile.outer), resolution)?;
        let cutouts = ContourSet::from_filled_contours(&profile.cutouts, resolution)?;
        substrate.union_assign(&outer.difference(&cutouts)?)?;
    }
    ensure!(!substrate.is_empty(), "missing board profile");
    let original_rings = substrate.rings.clone();
    let mut regions = Vec::new();
    let mut identities = Vec::new();
    let mut overlays = Vec::new();
    let mut missing = Vec::new();
    for component in &courtyards.components {
        if component.dnp || component.physical_role == "board-feature" {
            continue;
        }
        for envelope in &component.envelopes {
            let region = if let Some(circle) = &envelope.circle {
                ensure!(
                    envelope.rings.is_empty(),
                    "ambiguous circle/polygon courtyard"
                );
                let contour = shapes::circle(2.0 * circle.radius_mm)
                    .context("invalid courtyard circle")?
                    .transformed(Affine2::translation(Point::new(
                        circle.center[0],
                        circle.center[1],
                    )));
                ContourSet::from_filled_contours(&[contour], resolution)?
            } else {
                ContourSet::from_rings(envelope.rings.clone(), FillRule::EvenOdd, resolution)?
            };
            ensure!(
                !region.is_empty(),
                "empty accepted courtyard {}",
                component.id
            );
            let id = format!("{} / {} courtyard", component.id, envelope.side);
            overlays.push(
                json!({"id":id,"component":component.id,"side":envelope.side,"path":path(&region)}),
            );
            identities.push(id);
            regions.push(region);
        }
        if component.physical_role != "component"
            || !component.issues.is_empty()
            || component.envelopes.is_empty()
        {
            missing.push(format!("{}: {}", component.id, component.issues.join("; ")));
        }
    }
    let known = identities
        .iter()
        .zip(&regions)
        .map(|(id, region)| OutlineObstacle {
            id,
            region: Some(region),
        })
        .collect::<Vec<_>>();
    let mut complete = identities
        .iter()
        .zip(&regions)
        .map(|(id, region)| OutlineObstacle {
            id,
            region: Some(region),
        })
        .collect::<Vec<_>>();
    complete.extend(
        missing
            .iter()
            .map(|id| OutlineObstacle { id, region: None }),
    );
    let mut scenarios = Vec::new();
    for &footprint in footprints {
        // This guard is numerical, not a manufacturing clearance. Reserve one
        // thousandth of the explicit preparation budget, and disclose it.
        let tolerance = QueryTolerance {
            boundary_mm: substrate.uncertainty_mm,
            numerical_mm: resolution.accuracy.max_error_mm() / 1000.0,
        };
        let best_effort = eligible_outline(&substrate, &known, footprint, tolerance)?;
        let strict = eligible_outline(&substrate, &complete, footprint, tolerance)?;
        ensure!(
            best_effort.len() == strict.len(),
            "missing evidence changed geometric partition"
        );
        let mut totals = [0.0; 3];
        let intervals = best_effort.iter().zip(&strict).map(|(interval, strict)| {
            let state = match interval.state { OutlineState::Eligible => 0, OutlineState::Blocked => 1, OutlineState::Unknown => 2 };
            totals[state] += interval.end_mm - interval.start_mm;
            json!({
                "component":interval.boundary.component,"ring":interval.boundary.ring,"edge":interval.edge,
                "start_mm":interval.start_mm,"end_mm":interval.end_mm,
                "start":[interval.start.x,interval.start.y],"end":[interval.end.x,interval.end.y],
                "state":format!("{:?}",interval.state),"strict_state":format!("{:?}",strict.state),
                "landing":format!("{:?}",interval.landing),
                "sources":interval.obstacles.iter().map(|&i|known[i].id).collect::<Vec<_>>(),
                "uncertainty_mm":interval.uncertainty_mm,
            })
        }).collect::<Vec<_>>();
        scenarios.push(json!({"width_mm":footprint.width_mm,"inward_mm":footprint.inward_mm,"outward_mm":footprint.outward_mm,"numerical_mm":tolerance.numerical_mm,"totals_mm":totals,"intervals":intervals}));
    }
    ensure!(
        substrate.rings == original_rings,
        "outline filter changed substrate"
    );
    let bounds = substrate.bbox();
    let review_bounds = regions
        .iter()
        .fold(bounds, |bounds, region| bounds.union(region.bbox()));
    Ok(json!({
        "name":courtyards.name,"source":source,"substrate":path(&substrate),
        "substrate_rings":substrate.rings,"substrate_uncertainty_mm":substrate.uncertainty_mm,
        "bounds":[bounds.min.x,bounds.min.y,bounds.max.x,bounds.max.y],
        "review_bounds":[review_bounds.min.x,review_bounds.min.y,review_bounds.max.x,review_bounds.max.y],
        "obstacles":overlays,"missing":missing,"scenarios":scenarios,
        "diagnostics":design.geometry.diagnostics.iter().map(ToString::to_string).collect::<Vec<_>>(),
    }))
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 7,
        "usage: outline_review EXPORT_DIR COURTYARD_DIR OUTPUT_DIR WIDTHS_MM INWARD_MM OUTWARD_MM ACCURACY_MM\nWidths are comma-separated, e.g. 2,3,5; every dimension is an explicit review input."
    );
    let footprints = args[3]
        .split(',')
        .map(|width| {
            Ok(OutlineFootprint {
                width_mm: width.parse()?,
                inward_mm: args[4].parse()?,
                outward_mm: args[5].parse()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let resolution = Resolution::new(0.0, GeometryAccuracy::new(args[6].parse()?)?);
    let mut paths = fs::read_dir(&args[1])?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    });
    paths.sort();
    ensure!(!paths.is_empty(), "no extracted courtyard inputs");
    let mut boards = Vec::new();
    for source_path in paths {
        let source: Value = serde_json::from_slice(&fs::read(&source_path)?)?;
        let xml = Path::new(&args[0])
            .join(source_path.file_stem().unwrap())
            .with_extension("xml");
        boards.push(
            review(&xml, source, &footprints, resolution)
                .with_context(|| format!("review {}", source_path.display()))?,
        );
    }
    fs::create_dir_all(&args[2])?;
    let output = Path::new(&args[2]);
    fs::write(
        output.join("data.json"),
        serde_json::to_vec(
            &json!({"accuracy_mm":resolution.accuracy.max_error_mm(),"boards":boards}),
        )?,
    )?;
    fs::write(
        output.join("index.html"),
        include_str!("outline_review/index.html"),
    )?;
    Ok(())
}
