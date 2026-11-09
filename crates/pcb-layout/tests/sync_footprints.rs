use anyhow::{Context, Result};
use assert_fs::TempDir;
use assert_fs::prelude::*;
use pcb_kicad::footprint::embed_step_in_footprint;
use pcb_layout::{LayoutOptions, process_layout};
use pcb_sexpr::kicad::footprint::embedded_file_checksum;
use pcb_zen_core::{DefaultFileProvider, Diagnostics};

use crate::helpers::*;

fn prepare_simple_workspace() -> Result<(TempDir, pcb_zen_core::resolution::ResolutionResult)> {
    let temp = TempDir::new()?;
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

/// UUIDs of a footprint and its pads, which KiCad DRC exclusions reference.
fn footprint_and_pad_uuids(board: &str, reference: &str) -> Result<Vec<String>> {
    let uuid = |item: &[pcb_sexpr::Sexpr]| {
        pcb_sexpr::find_child_list(item, "uuid")
            .and_then(|uuid| uuid.get(1)?.as_atom())
            .map(str::to_owned)
    };
    let board = pcb_sexpr::parse(board)?;
    let footprint = board
        .find_all_lists("footprint")
        .into_iter()
        .find(|footprint| {
            pcb_sexpr::find_all_child_lists(footprint, "property")
                .iter()
                .any(|property| {
                    property.get(1).and_then(pcb_sexpr::Sexpr::as_atom) == Some("Reference")
                        && property.get(2).and_then(pcb_sexpr::Sexpr::as_atom) == Some(reference)
                })
        })
        .with_context(|| format!("Footprint {reference} not found"))?;
    std::iter::once(footprint)
        .chain(pcb_sexpr::find_all_child_lists(footprint, "pad"))
        .map(|item| uuid(item).context("missing uuid"))
        .collect()
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

#[test]
fn colon_paths_preserve_existing_board_and_repeat_sync() -> Result<()> {
    use pcb_sch::kicad_identity::footprint_kiid_path;
    use pcb_sexpr::board::{is_footprint_kiid_path, is_footprint_path_property, is_group_name};
    use std::collections::HashMap;

    let (temp, resolution) = prepare_simple_workspace()?;
    let zen_file = temp.path().join("MyBoard.zen");
    let source =
        std::fs::read_to_string(&zen_file)?.replace("name=\"BMI270\"", "name=\"Block:BMI270\"");
    std::fs::write(&zen_file, source)?;
    let module_file = temp.path().join("BMI270.zen");
    let source = std::fs::read_to_string(&module_file)?.replace("name=\"IC\"", "name=\"Chip:IC\"");
    std::fs::write(module_file, source)?;
    let schematic = evaluate_board(&zen_file, resolution)?;
    let mut diagnostics = Diagnostics::default();
    let layout = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?.unwrap();
    add_test_track(&layout.pcb_file)?;
    process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?;
    let original = std::fs::read_to_string(&layout.pcb_file)?;
    let project_file = layout.pcb_file.with_extension("kicad_pro");
    let project = std::fs::read(&project_file)?;

    let mut checked = Diagnostics::default();
    pcb_layout::check_layout_sync(&schematic, &mut checked)?;
    assert!(checked.diagnostics.is_empty(), "{checked:?}");

    // Rewrite the board the way older syncs stored it: paths truncated after their last colon.
    let board = pcb_sexpr::parse(&original)?;
    let mut patches = pcb_sexpr::PatchSet::default();
    let mut links = HashMap::new();
    board.walk_strings(|value, span, ctx| {
        if (is_footprint_path_property(&ctx) || is_group_name(&ctx))
            && let Some((_, truncated)) = value.rsplit_once(':')
        {
            patches.replace_string(span, truncated);
            links.insert(footprint_kiid_path(value), footprint_kiid_path(truncated));
        }
    });
    board.walk_strings(|value, span, ctx| {
        if is_footprint_kiid_path(&ctx)
            && let Some(link) = links.get(value)
        {
            patches.replace_string(span, link);
        }
    });
    let mut legacy = Vec::new();
    patches.write_to(&original, &mut legacy)?;
    assert_ne!(original.as_bytes(), legacy);
    std::fs::write(&layout.pcb_file, &legacy)?;

    // Sync restores the full paths in place, and syncing again changes nothing.
    for _ in 0..2 {
        process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?;
        assert_eq!(std::fs::read_to_string(&layout.pcb_file)?, original);
        assert_eq!(std::fs::read(&project_file)?, project);
        let mut checked = Diagnostics::default();
        pcb_layout::check_layout_sync(&schematic, &mut checked)?;
        assert!(checked.diagnostics.is_empty(), "{checked:?}");
    }
    Ok(())
}

#[test]
fn sync_footprints_reloads_same_fpid_models_and_preserves_board_state() -> Result<()> {
    let (temp, resolution) = prepare_simple_workspace()?;
    let zen_file = temp.path().join("MyBoard.zen");
    let footprint_file = temp.path().join("eda/BMI270.kicad_mod");
    let old_step = b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('old model'),'2;1');\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";
    let new_step = b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('new model'),'2;1');\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";
    let old_checksum = embedded_file_checksum(old_step);
    let new_checksum = embedded_file_checksum(new_step);

    let source = std::fs::read_to_string(&footprint_file)?;
    let source = embed_step_in_footprint(&source, old_step, "BMI270.step")?;
    std::fs::write(&footprint_file, source)?;

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
    assert!(initial_board.contains(&old_checksum));
    add_test_track(&initial.pcb_file)?;

    let source = std::fs::read_to_string(&footprint_file)?;
    let updated_source = source.replace("(xyz -90 0 0)", "(xyz -45 0 0)");
    assert_ne!(
        source, updated_source,
        "test fixture model transform was not found"
    );
    let updated_source = embed_step_in_footprint(&updated_source, new_step, "BMI270.step")?;
    std::fs::write(&footprint_file, updated_source)?;

    let schematic = evaluate_board(&zen_file, resolution.clone())?;
    let mut diagnostics = Diagnostics::default();
    let unchanged = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
        .context("unchanged layout was not generated")?;
    let unchanged_board = std::fs::read_to_string(&unchanged.pcb_file)?;
    assert!(unchanged_board.contains("(xyz -90 0 0)"));
    assert!(!unchanged_board.contains("(xyz -45 0 0)"));
    assert!(unchanged_board.contains(&old_checksum));
    assert!(!unchanged_board.contains(&new_checksum));
    let unchanged_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&unchanged.snapshot_file)?)?;
    let unchanged_tracks = unchanged_snapshot["tracks"].clone();
    assert_eq!(unchanged_tracks.as_array().map(Vec::len), Some(1));

    let schematic = evaluate_board(&zen_file, resolution.clone())?;
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
    assert!(synced_board.contains(&new_checksum));
    assert!(!synced_board.contains(&old_checksum));
    let synced_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&synced.snapshot_file)?)?;
    let synced_ic1 = footprint_snapshot(&synced_snapshot, "IC1")?;
    assert_eq!(placement_state(synced_ic1), initial_placement);
    assert_eq!(synced_ic1["pads"], initial_ic1["pads"]);
    assert_eq!(synced_snapshot["tracks"], unchanged_tracks);
    assert_eq!(
        footprint_and_pad_uuids(&synced_board, "IC1")?,
        footprint_and_pad_uuids(&unchanged_board, "IC1")?
    );

    let schematic = evaluate_board(&zen_file, resolution)?;
    let mut diagnostics = Diagnostics::default();
    let reloaded = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
        .context("reloaded layout was not generated")?;
    let reloaded_board = std::fs::read_to_string(&reloaded.pcb_file)?;
    assert!(reloaded_board.contains(&new_checksum));
    assert!(!reloaded_board.contains(&old_checksum));

    Ok(())
}

const OLD_STEP: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('old model'),'2;1');\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";
const NEW_STEP: &[u8] = b"ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION(('new model'),'2;1');\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";

/// Embed the old model in the fixture footprint, and write a new version of it under another
/// FPID that carries different model data under the same STEP filename.
fn write_footprint_versions(temp: &TempDir) -> Result<()> {
    let footprint_file = temp.path().join("eda/BMI270.kicad_mod");
    let source = std::fs::read_to_string(&footprint_file)?;
    std::fs::write(
        &footprint_file,
        embed_step_in_footprint(&source, OLD_STEP, "BMI270.step")?,
    )?;
    let new_source = source.replace("(xyz -90 0 0)", "(xyz -45 0 0)");
    assert_ne!(
        source, new_source,
        "test fixture model transform was not found"
    );
    std::fs::write(
        temp.path().join("eda/BMI270_v2.kicad_mod"),
        embed_step_in_footprint(&new_source, NEW_STEP, "BMI270.step")?,
    )?;
    Ok(())
}

fn use_new_footprint_version(module_file: &std::path::Path) -> Result<()> {
    let source = std::fs::read_to_string(module_file)?;
    let updated = source.replace("eda/BMI270.kicad_mod", "eda/BMI270_v2.kicad_mod");
    assert_ne!(source, updated, "footprint reference was not found");
    std::fs::write(module_file, updated)?;
    Ok(())
}

#[test]
fn fpid_change_takes_new_embedded_model_without_flag() -> Result<()> {
    let (temp, resolution) = prepare_simple_workspace()?;
    let zen_file = temp.path().join("MyBoard.zen");
    write_footprint_versions(&temp)?;
    let old_checksum = embedded_file_checksum(OLD_STEP);
    let new_checksum = embedded_file_checksum(NEW_STEP);

    let schematic = evaluate_board(&zen_file, resolution.clone())?;
    let mut diagnostics = Diagnostics::default();
    let initial = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
        .context("initial layout was not generated")?;
    assert!(std::fs::read_to_string(&initial.pcb_file)?.contains(&old_checksum));
    add_test_track(&initial.pcb_file)?;
    process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?;
    let before_snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&initial.snapshot_file)?)?;
    let before_ic1 = footprint_snapshot(&before_snapshot, "IC1")?;
    assert_eq!(before_snapshot["tracks"].as_array().map(Vec::len), Some(1));

    use_new_footprint_version(&temp.path().join("BMI270.zen"))?;
    let schematic = evaluate_board(&zen_file, resolution)?;
    let mut diagnostics = Diagnostics::default();
    let updated = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
        .context("updated layout was not generated")?;

    let board = std::fs::read_to_string(&updated.pcb_file)?;
    assert!(board.contains("BMI270_v2"));
    assert!(board.contains("(xyz -45 0 0)"));
    assert!(!board.contains("(xyz -90 0 0)"));
    assert!(board.contains(&new_checksum));
    assert!(!board.contains(&old_checksum));
    let snapshot: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&updated.snapshot_file)?)?;
    let ic1 = footprint_snapshot(&snapshot, "IC1")?;
    assert_eq!(placement_state(ic1), placement_state(before_ic1));
    assert_eq!(snapshot["tracks"], before_snapshot["tracks"]);
    Ok(())
}

#[test]
fn fpid_change_refuses_model_shared_with_kept_footprint() -> Result<()> {
    let (temp, resolution) = prepare_simple_workspace()?;
    let zen_file = temp.path().join("MyBoard.zen");
    write_footprint_versions(&temp)?;
    // A second sensor on the same footprint, in its own module so it can change version alone.
    std::fs::copy(
        temp.path().join("BMI270.zen"),
        temp.path().join("BMI270B.zen"),
    )?;
    let board_source = std::fs::read_to_string(&zen_file)?.replace(
        "Layout(",
        "BMI270B = Module(\"BMI270B.zen\")\n\nBMI270B(\n    name=\"BMI270B\",\n    POWER=power,\n    GND=gnd,\n    I2C=i2c,\n)\n\nLayout(",
    );
    std::fs::write(&zen_file, board_source)?;

    let schematic = evaluate_board(&zen_file, resolution.clone())?;
    let mut diagnostics = Diagnostics::default();
    let initial = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
        .context("initial layout was not generated")?;
    let before = std::fs::read_to_string(&initial.pcb_file)?;

    use_new_footprint_version(&temp.path().join("BMI270B.zen"))?;
    let schematic = evaluate_board(&zen_file, resolution)?;
    let mut diagnostics = Diagnostics::default();
    let error = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)
        .expect_err("layout sync should refuse the shared model change")
        .to_string();
    assert!(error.contains("Embedded model conflict"), "{error}");
    assert!(error.contains("BMI270.step"), "{error}");
    assert!(error.contains("IC1") && error.contains("IC2"), "{error}");
    assert!(error.contains("different STEP filename"), "{error}");
    assert_eq!(std::fs::read_to_string(&initial.pcb_file)?, before);
    Ok(())
}

/// Add J1, a copy of IC1 that sync does not manage. Without its Path property and KIID path it
/// stays on the board; with its Path property and a different KIID path, sync deletes it.
fn add_unmanaged_copy_of_ic1(
    pcb_file: &std::path::Path,
    keep_path_property: bool,
) -> Result<String> {
    let board = std::fs::read_to_string(pcb_file)?;
    let root = pcb_sexpr::parse(&board)?;
    let ic1 = root
        .as_list()
        .context("board is not a list")?
        .iter()
        .find(|node| {
            node.as_list().is_some_and(|items| {
                items.first().and_then(pcb_sexpr::Sexpr::as_sym) == Some("footprint")
                    && board[node.span.start..node.span.end].contains("\"IC1\"")
            })
        })
        .context("IC1 not found")?;
    let mut patches = pcb_sexpr::PatchSet::default();
    for item in ic1.as_list().context("IC1 is not a list")? {
        let Some(list) = item.as_list() else { continue };
        let tag = list.first().and_then(pcb_sexpr::Sexpr::as_sym);
        let span = pcb_sexpr::Span::new(
            item.span.start - ic1.span.start,
            item.span.end - ic1.span.start,
        );
        if tag == Some("path") {
            let replacement = if keep_path_property {
                "(path \"/00000000-0000-0000-0000-000000000001\")"
            } else {
                ""
            };
            patches.replace_raw(span, replacement.to_string());
        } else if !keep_path_property
            && tag == Some("property")
            && list.get(1).and_then(pcb_sexpr::Sexpr::as_atom) == Some("Path")
        {
            patches.replace_raw(span, String::new());
        }
    }
    let mut copy = Vec::new();
    patches.write_to(&board[ic1.span.start..ic1.span.end], &mut copy)?;
    let copy = String::from_utf8(copy)?.replace("\"IC1\"", "\"J1\"");
    let end = board
        .rfind(')')
        .context("missing board closing parenthesis")?;
    let mut board = board;
    board.insert_str(end, &format!("\t{copy}\n"));
    std::fs::write(pcb_file, &board)?;
    Ok(board)
}

#[test]
fn fpid_change_checks_models_of_unmanaged_footprints_that_stay() -> Result<()> {
    for keep_path_property in [false, true] {
        let (temp, resolution) = prepare_simple_workspace()?;
        let zen_file = temp.path().join("MyBoard.zen");
        write_footprint_versions(&temp)?;

        let schematic = evaluate_board(&zen_file, resolution.clone())?;
        let mut diagnostics = Diagnostics::default();
        let initial = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics)?
            .context("initial layout was not generated")?;
        let board = add_unmanaged_copy_of_ic1(&initial.pcb_file, keep_path_property)?;

        use_new_footprint_version(&temp.path().join("BMI270.zen"))?;
        let schematic = evaluate_board(&zen_file, resolution)?;
        let mut diagnostics = Diagnostics::default();
        let result = process_layout(&schematic, LayoutOptions::default(), &mut diagnostics);
        if keep_path_property {
            // Sync deletes the copy, so nothing keeps the old model.
            result?;
            let synced = std::fs::read_to_string(&initial.pcb_file)?;
            assert!(!synced.contains("\"J1\""));
            assert!(synced.contains(&embedded_file_checksum(NEW_STEP)));
        } else {
            let error = result
                .expect_err("layout sync should refuse the shared model change")
                .to_string();
            assert!(error.contains("Embedded model conflict"), "{error}");
            assert!(error.contains("IC1") && error.contains("J1"), "{error}");
            assert_eq!(std::fs::read_to_string(&initial.pcb_file)?, board);
        }
    }
    Ok(())
}
