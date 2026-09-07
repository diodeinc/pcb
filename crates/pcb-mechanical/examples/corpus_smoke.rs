//! Native, source-independent software smoke. No fixture/load qualification.
use pcb_corpus::{Fixture, Report, Status};
use pcb_elastic::{DMatrix, DVector, Model, Tolerances};
use pcb_ir::geom::mesh::MeshOptions;
use pcb_mechanical::{Laminate, Load, Panel, ProcessCase};

fn smoke(f: &Fixture) -> Report {
    let mut report = Report::outcome(
        &f.id,
        "mechanical-software-smoke",
        Status::Unavailable,
        "UNVALIDATED: no actual laminate, rail/frame/tab geometry, fixtures or process loads supplied",
    );
    let result = (|| {
        pcb_corpus::validate(f)?;
        // Substrate already excludes source profile voids. Do not reinterpret
        // removal overlays with unknown spans as through-board holes.
        let substrate = pcb_corpus::region(&f.substrate, f.tolerance_mm);
        let panel = Panel::from_regions(
            &[substrate],
            &pcb_ir::geom::ContourSet::empty(f.tolerance_mm),
            &[],
        )
        .map_err(|e| e.to_string())?;
        // Synthetic D=1, nu=0 only exercises the operator, not an FR4 preset.
        let laminate = Laminate {
            thickness_mm: 1.,
            plane_stress: DMatrix::from_diagonal(&DVector::from_vec(vec![12., 12., 6.])),
            evidence: None,
        };
        let d = panel
            .discretize(
                &laminate,
                MeshOptions {
                    max_area_mm2: 25.,
                    min_angle_degrees: 20.,
                    max_additional_vertices: 3000,
                },
                1500,
            )
            .map_err(|e| e.to_string())?;
        let p = d
            .problem(&ProcessCase {
                name: "synthetic free-body pressure; NOT a manufacturing load case".into(),
                loads: vec![Load::Pressure(1e-6)],
                rails: vec![],
                tooling: vec![],
                evidence: None,
            })
            .map_err(|e| e.to_string())?;
        let model = Model::new(
            p.scales.clone(),
            &p.contributions,
            Tolerances {
                rank_relative: 1e-12,
                rank_absolute: 0.,
                residual_relative: 1e-10,
                residual_absolute: 1e-10,
            },
        )
        .map_err(|e| e.to_string())?;
        let response = model
            .evaluate(&[], &p.forces, &p.prescribed)
            .map_err(|e| e.to_string())?;
        // A free body under net transverse force cannot equilibrate. This is
        // an expected diagnostic, NOT a supported manufacturing solution.
        let expected_modes = 3 * panel.substrate.connected_components().len();
        if response.status != pcb_elastic::Status::SingularIncompatible
            || response.unsupported_modes.len() != expected_modes
        {
            return Err(format!(
                "unexpected free-body result {:?}, {} modes (expected {expected_modes})",
                response.status,
                response.unsupported_modes.len()
            ));
        }
        Ok::<_, String>(format!(
            "software-only expected SingularIncompatible; DOFs={}, modes={}, area_mm2={}, quality={:?}; physical metrics unavailable",
            p.scales.len(),
            response.unsupported_modes.len(),
            d.mesh().quality.area_mm2,
            d.mesh().quality,
        ))
    })();
    match result {
        Ok(message) => {
            report.status = Status::Completed;
            report.message.push_str(&format!("; {message}"));
        }
        Err(message) => report
            .message
            .push_str(&format!("; analysis unavailable: {message}")),
    }
    report
}

fn main() -> std::process::ExitCode {
    let paths: Vec<_> = std::env::args_os().skip(1).collect();
    if paths.is_empty() {
        anstream::eprintln!(
            "usage: cargo run --release -p pcb-mechanical --example corpus_smoke -- CANONICAL.json[.zst] ..."
        );
        return std::process::ExitCode::FAILURE;
    }
    let mut complete = true;
    for path in paths {
        let report = match pcb_corpus::load(
            std::path::Path::new(&path),
            "mechanical-software-smoke",
        ) {
            Ok(f) => {
                anstream::println!(
                    "source={} revision={} path={} sha256={:?}; source_thickness_mm={}; physical_model={}; full material/envelope diagnostics remain in canonical fixture; limitations={:?}",
                    f.provenance.repository,
                    f.provenance.revision,
                    f.provenance.path,
                    f.provenance.sha256,
                    f.evidence["overall_thickness_mm"],
                    f.evidence["physical_model"],
                    f.provenance.limitations
                );
                smoke(&f)
            }
            Err(report) => *report,
        };
        complete &= report.status == Status::Completed;
        anstream::println!(
            "{}: {:?}: {}",
            report.fixture,
            report.status,
            report.message
        );
    }
    if complete {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}
