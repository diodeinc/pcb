use anyhow::{Context, Result};
use assert_fs::TempDir;
use assert_fs::prelude::*;
use pcb_layout::{LayoutOptions, process_layout};
use pcb_zen_core::{DefaultFileProvider, Diagnostics};

use crate::helpers::*;

fn prepare_simple_workspace() -> Result<(TempDir, pcb_zen_core::resolution::ResolutionResult)> {
    let temp = TempDir::new()?.into_persistent();
    temp.copy_from(get_resource_path("simple"), &["**/*", "!.pcb/cache/**/*"])?;
    let workspace_info = pcb_zen::get_workspace_info(&DefaultFileProvider::new(), temp.path())?;
    let resolution = pcb_zen::resolve_workspace_dependencies(workspace_info, temp.path(), false)?;
    Ok((temp, resolution))
}

fn evaluate_board(
    zen_file: &std::path::Path,
    resolution: pcb_zen_core::resolution::ResolutionResult,
) -> Result<pcb_sch::Schematic> {
    let (output, diagnostics) = pcb_zen::run(zen_file, resolution, Default::default()).unpack();
    if !diagnostics.is_empty() {
        for diagnostic in diagnostics {
            eprintln!("{diagnostic:?}");
        }
    }
    output.context("Zen evaluation should produce a schematic")
}

fn footprint_snapshot<'a>(
    snapshot: &'a serde_json::Value,
    reference: &str,
) -> Result<&'a serde_json::Value> {
    snapshot["footprints"]
        .as_array()
        .and_then(|footprints| {
            footprints
                .iter()
                .find(|footprint| footprint["reference"].as_str() == Some(reference))
        })
        .with_context(|| format!("Footprint {reference} not found"))
}

fn placement_state(footprint: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "position": footprint["position"],
        "orientation": footprint["orientation"],
        "layer": footprint["layer"],
        "locked": footprint["locked"],
        "group": footprint["group"],
    })
}

fn add_test_track(pcb_file: &std::path::Path) -> Result<()> {
    let mut board = std::fs::read_to_string(pcb_file)?;
    let end = board
        .rfind(')')
        .context("missing board closing parenthesis")?;
    board.insert_str(
        end,
        r#"
	(segment
		(start 148.5 105)
		(end 149.5 105)
		(width 0.2)
		(layer "F.Cu")
		(net 1)
		(uuid "35c6dc7d-77c1-44a1-a273-0e27aa0dbf50")
	)
"#,
    );
    std::fs::write(pcb_file, board)?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
#[test]
fn sync_footprints_reloads_same_fpid_models_and_preserves_board_state() -> Result<()> {
    let (temp, resolution) = prepare_simple_workspace()?;
    let zen_file = temp.path().join("MyBoard.zen");
    let footprint_file = temp.path().join("eda/BMI270.kicad_mod");

    let schematic = evaluate_board(&zen_file, resolution.clone())?;
    let mut diagnostics = Diagnostics::default();
    let initial = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
        .context("initial layout was not generated")?;
    let initial_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&initial.snapshot_file)?)?;
    let initial_ic1 = footprint_snapshot(&initial_snapshot, "IC1")?;
    let initial_placement = placement_state(initial_ic1);
    let initial_board = std::fs::read_to_string(&initial.pcb_file)?;
    assert!(initial_board.contains("(xyz -90 0 0)"));
    add_test_track(&initial.pcb_file)?;

    let source = std::fs::read_to_string(&footprint_file)?;
    let updated_source = source.replace("(rotate (xyz -90 0 0))", "(rotate (xyz -45 0 0))");
    assert_ne!(
        source, updated_source,
        "test fixture model transform was not found"
    );
    std::fs::write(&footprint_file, updated_source)?;

    let schematic = evaluate_board(&zen_file, resolution.clone())?;
    let mut diagnostics = Diagnostics::default();
    let unchanged = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
        .context("unchanged layout was not generated")?;
    let unchanged_board = std::fs::read_to_string(&unchanged.pcb_file)?;
    assert!(unchanged_board.contains("(xyz -90 0 0)"));
    assert!(!unchanged_board.contains("(xyz -45 0 0)"));
    let unchanged_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&unchanged.snapshot_file)?)?;
    let unchanged_tracks = unchanged_snapshot["tracks"].clone();
    assert_eq!(unchanged_tracks.as_array().map(Vec::len), Some(1));

    let schematic = evaluate_board(&zen_file, resolution)?;
    let mut diagnostics = Diagnostics::default();
    let synced = process_layout(
        &schematic,
        LayoutOptions {
            sync_footprints: true,
            ..Default::default()
        },
        &mut diagnostics,
    )?
    .context("synced layout was not generated")?;

    let synced_board = std::fs::read_to_string(&synced.pcb_file)?;
    assert!(synced_board.contains("(xyz -45 0 0)"));
    assert!(!synced_board.contains("(xyz -90 0 0)"));
    let synced_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&synced.snapshot_file)?)?;
    let synced_ic1 = footprint_snapshot(&synced_snapshot, "IC1")?;
    assert_eq!(placement_state(synced_ic1), initial_placement);
    assert_eq!(synced_ic1["pads"], initial_ic1["pads"]);
    assert_eq!(synced_snapshot["tracks"], unchanged_tracks);

    Ok(())
}
