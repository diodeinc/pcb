use super::*;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Prefixes of the printed diagnostics paths. The paths are the only handle callers have on files
/// written outside the output repository, so each literal is named once here rather than repeated at
/// its use sites. `tests/import.rs` carries its own copies: `pcbc` is a `[[bin]]`-only crate, so the
/// integration test cannot import these.
const EXTRACTION_REPORT_PREFIX: &str = "Wrote import extraction report to ";
const VALIDATION_DIAGNOSTICS_PREFIX: &str = "Wrote import validation diagnostics to ";

pub(super) fn execute(args: ImportArgs) -> Result<()> {
    let ctx = ImportContext::new(args)?;

    let discovered = Discovered::run(ctx)?;
    let validated = Validated::run(discovered)?;
    let extracted = Extracted::run(validated)?;
    let hierarchized = Hierarchized::run(extracted);
    let analyzed = Analyzed::run(hierarchized);
    let materialized = Materialized::run(analyzed)?;

    generate_and_report(materialized)
}

fn generate_and_report(materialized: Materialized) -> Result<()> {
    let Materialized {
        ctx,
        selection,
        validation,
        ir,
        board,
        mut writer,
    } = materialized;

    let mut generation = generate::generate(&board, &selection.board_name, &ir, true, &mut writer)?;

    let mut kept_existing = writer.kept_paths();
    let mut validated_zen = generation.registry_reused_entrypoints.clone();
    validated_zen.extend(kept_zen(&kept_existing));
    validated_zen.sort();
    validated_zen.dedup();
    // Printed before validation rather than after `commit`: staged-board validation can reject a kept
    // file, and the user needs to know a file was kept — and that `--force` is the remedy — on the
    // failure path too, not only when the import lands.
    if let Some(warning) = format_kept_existing_warning(&kept_existing) {
        eprintln!("{warning}");
    }
    // A validation failure is recorded rather than returned here, so the extraction report below is
    // written for the failed run too. That report is the only structured account of what import
    // produced; returning early left the hard failure legible on stderr alone, and left a report from
    // a *previous* successful run in place to be mistaken for this one's.
    let mut validation_failure: Option<anyhow::Error> = None;
    if let Err(error) = reuse_validate::validate_reused_zen(
        &board,
        &ir,
        &validated_zen,
        &generation.registry_reused_entrypoints,
        &generation.expected_pins_by_refdes,
        &generation.instance_name_by_refdes,
    ) {
        if generation.registry_reused_entrypoints.is_empty() {
            validation_failure = Some(error);
        } else {
            log::debug!(
                "Staged-board validation failed with cached registry substitution: {error:#}"
            );
            eprintln!(
                "Staged-board validation failed; retrying the import without cached registry substitutions"
            );
            // The retry disables reuse altogether, so it rediscovers no ambiguity — but the components
            // the first pass found ambiguous were ambiguous all the same and must stay in the report.
            // Unlike the kept-file list below, this one is not re-derived by the retry.
            let first_pass_ambiguity = std::mem::take(&mut generation.registry_ambiguous_by_refdes);
            writer.discard_generated(&board.board_zen)?;
            generation =
                generate::generate(&board, &selection.board_name, &ir, false, &mut writer)?;
            for (refdes, count) in first_pass_ambiguity {
                generation
                    .registry_ambiguous_by_refdes
                    .entry(refdes)
                    .or_insert(count);
            }
            // Authoritative: the retry regenerates every package and decides its collisions again, and
            // `discard_generated` dropped the first pass's keeps within that scope, so this is the
            // retry's own answer rather than the union of both passes.
            let retried_kept = writer.kept_paths();
            if retried_kept != kept_existing {
                // The list printed before validation described the first pass. Correct it rather than
                // leaving a stale claim that edits are not reflected.
                match format_kept_existing_warning(&retried_kept) {
                    Some(warning) => eprintln!("After the retry, the kept file(s) are:\n{warning}"),
                    None => {
                        eprintln!("The retry regenerated every file that was previously kept")
                    }
                }
            }
            kept_existing = retried_kept;
            if let Err(retry_error) = reuse_validate::validate_reused_zen(
                &board,
                &ir,
                &kept_zen(&kept_existing),
                &[],
                &generation.expected_pins_by_refdes,
                &generation.instance_name_by_refdes,
            ) {
                validation_failure = Some(retry_error);
            }
        }
    }

    let report = report::build_import_report(
        &ctx.paths,
        &selection,
        &validation,
        ir,
        &board,
        &report::ReportGenerationOutcome {
            registry_reused_entrypoints: &generation.registry_reused_entrypoints,
            sourcing_by_refdes: &generation.sourcing_by_refdes,
            kept_existing_files: &kept_existing,
            registry_ambiguous_by_refdes: &generation.registry_ambiguous_by_refdes,
            validation_failure: validation_failure
                .as_ref()
                .map(|error| format!("{error:#}")),
        },
    );
    // Non-fatal: the report describes the conversion rather than the design, so failing to write it must
    // not be what fails an import — and on the validation-failure path the import is failing anyway,
    // with its own error.
    if let Err(error) =
        report::write_import_extraction_report(&board.import_extraction_json, &report)
    {
        eprintln!("Failed to write import extraction report (continuing): {error:#}");
        // The report sits at a stable per-board path, so a failed write leaves the *previous* run's
        // report — or a half-written one, since the write truncates in place — to be read as describing
        // this run. Remove it rather than leave a lie in a documented location. Unconditional: a stale
        // report claiming a failure that has since been fixed misleads exactly as much as the reverse.
        let _ = std::fs::remove_file(&board.import_extraction_json);
    }

    // Each path is printed only if its write landed, so a warned-about failure is never followed by a
    // path that does not exist.
    for (prefix, path) in [
        (EXTRACTION_REPORT_PREFIX, &board.import_extraction_json),
        (
            VALIDATION_DIAGNOSTICS_PREFIX,
            &board.validation_diagnostics_json,
        ),
    ] {
        if let Some(line) = format_diagnostics_path(prefix, path) {
            eprintln!("{line}");
        }
    }

    // Returned after the report is written and its path announced, so the structured account of the
    // failure — including which refdes it concerns, in `validation_failure` — is on disk and findable
    // before the error reaches the user.
    if let Some(error) = validation_failure {
        return Err(error);
    }

    eprintln!("Wrote imported board to {}", board.board_zen.display());
    Ok(())
}

/// The line announcing a diagnostics file, or `None` when the file is not there.
///
/// Both diagnostics writes are non-fatal, so the announcement is conditional on the file existing:
/// the printed path is the user's only handle on it, and a path to a file that was never written is
/// worse than no path at all.
fn format_diagnostics_path(prefix: &str, path: &Path) -> Option<String> {
    path.is_file()
        .then(|| format!("{prefix}{}", path.display()))
}

/// The `.zen` subset of the kept paths, which is what reuse validation reasons about: an entrypoint
/// the staged build will actually load.
fn kept_zen(kept_existing: &[PathBuf]) -> Vec<PathBuf> {
    kept_existing
        .iter()
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("zen"))
        .cloned()
        .collect()
}

fn format_kept_existing_warning(kept_existing: &[PathBuf]) -> Option<String> {
    if kept_existing.is_empty() {
        return None;
    }
    let capped = &kept_existing[..kept_existing.len().min(16)];
    Some(format!(
        "Kept existing file(s) and discarded the newly generated version: {}\nEdits to the KiCad source are not reflected in those files; re-import with --force to overwrite them",
        output::join_paths(capped)
    ))
}

struct ImportContext {
    args: ImportArgs,
    paths: ImportPaths,
}

impl ImportContext {
    fn new(args: ImportArgs) -> Result<Self> {
        let paths = paths::resolve_paths(&args)?;
        Ok(Self { args, paths })
    }
}

struct Discovered {
    ctx: ImportContext,
    selection: ImportSelection,
}

impl Discovered {
    fn run(ctx: ImportContext) -> Result<Self> {
        let selection = discover::discover_and_select(&ctx.paths)?;
        Ok(Self { ctx, selection })
    }
}

struct Validated {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
}

impl Validated {
    fn run(discovered: Discovered) -> Result<Self> {
        let Discovered { ctx, selection } = discovered;
        let validation = validate::validate(&ctx.paths, &selection)?;
        Ok(Self {
            ctx,
            selection,
            validation,
        })
    }
}

struct Extracted {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Extracted {
    fn run(validated: Validated) -> Result<Self> {
        let Validated {
            ctx,
            selection,
            validation,
        } = validated;

        let ir = extract::extract_ir(&ctx.paths, &selection, &validation)?;

        Ok(Self {
            ctx,
            selection,
            validation,
            ir,
        })
    }
}

struct Hierarchized {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Hierarchized {
    fn run(extracted: Extracted) -> Self {
        let Extracted {
            ctx,
            selection,
            validation,
            ir,
        } = extracted;

        let hierarchy_plan = hierarchy::build_hierarchy_plan(&ir);
        let ir = ImportIr {
            hierarchy_plan,
            ..ir
        };

        Self {
            ctx,
            selection,
            validation,
            ir,
        }
    }
}

struct Analyzed {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
}

impl Analyzed {
    fn run(hierarchized: Hierarchized) -> Self {
        let Hierarchized {
            ctx,
            selection,
            validation,
            mut ir,
        } = hierarchized;

        registry_lookup::inherit_embedded_symbol_properties(
            &mut ir.components,
            &ir.schematic_lib_symbols,
        );
        let mut semantic = semantic::analyze(&ir);
        semantic.registry_mpn_lookup =
            registry_lookup::lookup_cached_registry_mpns(&ir.components, &ctx.paths.workspace_root);

        eprintln!(
            "Passive detection (2-pad only): R={} (h:{} m:{} l:{}), C={} (h:{} m:{} l:{}), unknown:{}, known-non-2-pad:{}, pad-count-unknown:{}",
            semantic.passives.summary.resistor_high
                + semantic.passives.summary.resistor_medium
                + semantic.passives.summary.resistor_low,
            semantic.passives.summary.resistor_high,
            semantic.passives.summary.resistor_medium,
            semantic.passives.summary.resistor_low,
            semantic.passives.summary.capacitor_high
                + semantic.passives.summary.capacitor_medium
                + semantic.passives.summary.capacitor_low,
            semantic.passives.summary.capacitor_high,
            semantic.passives.summary.capacitor_medium,
            semantic.passives.summary.capacitor_low,
            semantic.passives.summary.unknown,
            semantic.passives.summary.non_two_pad,
            semantic.passives.summary.unknown_pad_count,
        );

        if let Some(error) = &semantic.registry_mpn_lookup.lookup_error {
            eprintln!("Registry exact-MPN lookup skipped: {error}");
        } else if semantic.registry_mpn_lookup.cached_index_available {
            let candidate_count = semantic
                .registry_mpn_lookup
                .candidates_by_mpn
                .values()
                .map(Vec::len)
                .sum::<usize>();
            eprintln!(
                "Registry exact-MPN lookup: {} queried, {} matched, {} candidate module(s)",
                semantic.registry_mpn_lookup.queried_mpns.len(),
                semantic.registry_mpn_lookup.candidates_by_mpn.len(),
                candidate_count,
            );
        }

        let ir = ImportIr { semantic, ..ir };

        Self {
            ctx,
            selection,
            validation,
            ir,
        }
    }
}

struct Materialized {
    ctx: ImportContext,
    selection: ImportSelection,
    validation: ImportValidationRun,
    ir: ImportIr,
    board: MaterializedBoard,
    writer: output::ImportWriter,
}

impl Materialized {
    fn run(analyzed: Analyzed) -> Result<Self> {
        let Analyzed {
            ctx,
            selection,
            validation,
            ir,
        } = analyzed;

        let mut writer = output::ImportWriter::new(
            &ctx.paths.workspace_root,
            &selection.board_name,
            ctx.args.force,
        )?;
        // The archive is a copy of the input the user already has, so it is opt-in rather than part
        // of every import's output.
        let portable_kicad_project_zip = if ctx.args.archive_sources {
            let archive = writer
                .root()
                .join(format!("{}.kicad.archive.zip", selection.board_name));
            let bytes = portable::build_portable_zip(&selection.portable)
                .context("Failed to build portable KiCad project archive")?;
            // Through the writer, so the archive obeys the same no-overwrite rule as the sources: an
            // existing archive is kept unless `--force`, and the write lands by rename.
            writer
                .write(&archive, &bytes)
                .context("Failed to write portable KiCad project archive")?;
            Some(archive)
        } else {
            None
        };
        let board = materialize::materialize_board(
            &ctx.paths,
            &selection,
            &validation,
            portable_kicad_project_zip,
            &mut writer,
        )?;

        Ok(Self {
            ctx,
            selection,
            validation,
            ir,
            board,
            writer,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_warning_without_kept_files() {
        assert!(format_kept_existing_warning(&[]).is_none());
    }

    /// A diagnostics write that failed leaves nothing at the path, and the import must then say
    /// nothing about it rather than print a path the user cannot open.
    #[test]
    fn a_missing_diagnostics_file_is_not_announced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing.json");
        assert_eq!(format_diagnostics_path("Wrote ", &missing), None);

        let written = temp.path().join("written.json");
        std::fs::write(&written, "{}").expect("write");
        assert_eq!(
            format_diagnostics_path("Wrote ", &written),
            Some(format!("Wrote {}", written.display()))
        );
    }

    #[test]
    fn only_zen_paths_reach_reuse_validation() {
        let kept = [
            PathBuf::from("components/R/R.kicad_sym"),
            PathBuf::from("components/R/R.zen"),
            PathBuf::from("pcb.toml"),
        ];
        assert_eq!(kept_zen(&kept), vec![PathBuf::from("components/R/R.zen")]);
    }
}
