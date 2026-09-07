//! Thin source adapter only. Replay never calls this executable.
use anyhow::{Context, Result};
use pcb_corpus::{Fixture, Overlay, Provenance, VERSION};
use pcb_ir::{
    geom::{ContourSet, Resolution},
    import::ipc2581::import_design,
};
use serde_json::json;
use sha2::{Digest, Sha256};

fn rings(region: &ContourSet) -> Vec<pcb_ir::geom::region::Ring> {
    region.rings.clone()
}

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    anyhow::ensure!(
        args.len() == 4,
        "usage: extract SOURCE.xml PROVENANCE.json ID OUTPUT.json"
    );
    let source = std::fs::read(&args[0])?;
    let mut provenance: Provenance = serde_json::from_slice(&std::fs::read(&args[1])?)?;
    provenance.sha256 = Some(
        Sha256::digest(&source)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    );
    provenance.extraction = "pcb-ir import_design -> physical_board; board-local mm; even-odd flattened regions; corpus-v1".into();
    let ipc = ipc2581::Ipc2581::parse(std::str::from_utf8(&source)?)
        .context("malformed_source: IPC parsing")?;
    let resolution = Resolution::default();
    let design = import_design(&ipc, resolution).context("malformed_source: canonical import")?;
    let board = design
        .physical_board(resolution)
        .context("geometry_rejected: physical board extraction")?;
    let mut overlays = vec![Overlay {
        name: "profile-cutouts".into(),
        meaning: "Source profile voids; already excluded from substrate".into(),
        rings: rings(&board.profile_cutouts),
    }];
    for layer in &board.copper {
        overlays.push(Overlay {
            name: format!("copper {:?}", layer.layer),
            meaning: "Polarity-composed copper on a distinct source layer".into(),
            rings: rings(&layer.image),
        });
    }
    for layer in &board.removal_layers {
        overlays.push(Overlay {
            name: format!("composed drill/rout {:?}", layer.layer),
            meaning: format!(
                "Final polarity-composed removal artwork; not assumed through-board. Sources {:?}",
                layer.sources
            ),
            rings: rings(&layer.image),
        });
    }
    for hole in &board.holes {
        overlays.push(Overlay {
            name: format!("hole {:?}", hole.id),
            meaning: format!(
                "Source aperture BEFORE polarity composition, plating {:?}, span {:?}; not subtracted from substrate",
                hole.plating, hole.span
            ),
            rings: rings(&hole.image),
        });
    }
    for component in &board.components {
        for envelope in &component.envelopes {
            overlays.push(Overlay { name:format!("{} {:?} {:?}",component.designator.as_deref().unwrap_or("unnamed"),component.component,envelope.kind), meaning:format!("{:?}; view {:?}; status {:?}; side {:?}; population {:?}; not a measured body or inferred courtyard",envelope.kind,envelope.view,envelope.status,component.side,component.population), rings:rings(&envelope.image) });
        }
    }
    let evidence = json!({
        "board_step":board.step, "source_profiles":board.profiles,
        "substrate_uncertainty_mm":board.substrate.uncertainty_mm,
        "preparation_budget_mm":board.substrate.budget().max_error_mm(),
        "metadata":format!("{:?}",board.metadata),
        "overall_thickness_mm":board.metadata.overall_thickness_mm,
        "material_layers":board.metadata.layers.iter().map(|l| json!({
            "layer":ipc.resolve(l.layer_ref), "thickness_mm":l.thickness_mm,
            "mat_des":l.mat_des.map(|s|ipc.resolve(s)),
            "material":l.material.resolved().map(|s|ipc.resolve(*s)),
            "association":format!("{:?}",l.material),
            "spec":l.spec_ref.map(|s|ipc.resolve(s)),
            "spec_refs":l.spec_refs.iter().map(|s|ipc.resolve(*s)).collect::<Vec<_>>()
        })).collect::<Vec<_>>(),
        "diagnostics":board.diagnostics.iter().map(|d|format!("{d:?}")).collect::<Vec<_>>(),
        "source_diagnostics":board.source_diagnostics.iter().map(|d|format!("{d:?}")).collect::<Vec<_>>(),
        "layer_names":design.layer_definitions.iter().enumerate().map(|(i,l)|json!({"index":i,"name":ipc.resolve(l.name)})).collect::<Vec<_>>(),
        "physical_model":null,
        "note":"Substrate precedes drills/routs; unknown-span holes are retained separately. Metadata debug identities are snapshot-local, not material model parameters."
    });
    let fixture = Fixture {
        version: VERSION,
        id: args[2].clone(),
        provenance,
        tolerance_mm: board.substrate.tolerance(),
        flatten_mm: board.substrate.budget().max_error_mm(),
        substrate: rings(&board.substrate),
        removal: vec![],
        overlays,
        evidence,
    };
    pcb_corpus::validate(&fixture).map_err(anyhow::Error::msg)?;
    // Compact encoding keeps full geometry without decimation or sampling.
    let json = serde_json::to_vec(&fixture)?;
    let bytes = if args[3].ends_with(".zst") {
        zstd::encode_all(json.as_slice(), 19)?
    } else {
        json
    };
    std::fs::write(&args[3], bytes)?;
    Ok(())
}
