use anyhow::{Context, Result};
use clap::Args;
use log::debug;
use pcb_fmt::RuffFormatter;
use pcb_sexpr::formatter::{FormatMode, prettify};
use pcb_ui::prelude::*;
use similar::TextDiff;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::file_walker;

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Format .zen files")]
pub struct FmtArgs {
    /// .zen file or directory to format. Defaults to current directory.
    /// If this is an explicit KiCad S-expression file path, format only that file.
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub path: Option<PathBuf>,

    /// Check if files are formatted correctly without modifying them.
    /// Exit with non-zero code if any file needs formatting.
    #[arg(long)]
    pub check: bool,

    /// Show diffs instead of writing files
    #[arg(long)]
    pub diff: bool,
}

/// Format a single file using ruff formatter
fn format_zen_file(formatter: &RuffFormatter, file_path: &Path, op: FmtOp) -> Result<bool> {
    debug!("Formatting file: {}", file_path.display());

    match op {
        FmtOp::Check => formatter.check_file(file_path),
        FmtOp::Diff => {
            let diff = formatter.diff_file(file_path)?;
            if !diff.is_empty() {
                print!("{diff}");
            }
            Ok(true)
        }
        FmtOp::Write => {
            formatter.format_file(file_path)?;
            Ok(true)
        }
    }
}

/// Infer KiCad prettifier mode from file type.
fn infer_kicad_mode(file_path: &Path) -> Option<FormatMode> {
    let file_name = file_path
        .file_name()?
        .to_string_lossy()
        .to_ascii_lowercase();

    if file_name == "fp-lib-table" || file_name == "sym-lib-table" {
        return Some(FormatMode::LibraryTable);
    }

    let ext = file_path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase();
    match ext.as_str() {
        "kicad_pcb" | "kicad_sch" | "kicad_sym" | "kicad_mod" | "kicad_wks" | "kicad_dru" => {
            Some(FormatMode::Normal)
        }
        _ => None,
    }
}

#[derive(Debug, Clone)]
enum FmtTarget {
    Zen(PathBuf),
    Kicad { path: PathBuf, mode: FormatMode },
}

impl FmtTarget {
    fn path(&self) -> &Path {
        match self {
            Self::Zen(path) => path.as_path(),
            Self::Kicad { path, .. } => path.as_path(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FmtOp {
    Check,
    Diff,
    Write,
}

impl FmtOp {
    fn from_args(args: &FmtArgs) -> Self {
        if args.check {
            Self::Check
        } else if args.diff {
            Self::Diff
        } else {
            Self::Write
        }
    }

    fn spinner_suffix(self) -> &'static str {
        match self {
            Self::Check => "Checking format",
            Self::Diff => "Checking diff",
            Self::Write => "Formatting",
        }
    }

    fn is_check(self) -> bool {
        matches!(self, Self::Check)
    }
}

/// Format a single KiCad S-expression file using the KiCad-style formatter.
///
/// Returns:
/// - `true` in `--check` mode if the file needs formatting
/// - `true` in other modes if processing succeeded
fn format_kicad_file(file_path: &Path, op: FmtOp, mode: FormatMode) -> Result<bool> {
    let source = fs::read_to_string(file_path)
        .with_context(|| format!("Failed to read {}", file_path.display()))?;

    pcb_sexpr::parse(&source)
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| {
            format!(
                "Failed to parse KiCad S-expression file {}",
                file_path.display()
            )
        })?;

    let formatted = prettify(&source, mode);

    match op {
        FmtOp::Check => Ok(source != formatted),
        FmtOp::Diff => {
            if source != formatted {
                let diff = TextDiff::from_lines(source.as_str(), formatted.as_str());
                print!(
                    "{}",
                    diff.unified_diff().context_radius(3).header(
                        &format!("old/{}", file_path.display()),
                        &format!("new/{}", file_path.display())
                    )
                );
            }
            Ok(true)
        }
        FmtOp::Write => {
            if source != formatted {
                write_text_buffered(file_path, &formatted)
                    .with_context(|| format!("Failed to write {}", file_path.display()))?;
            }
            Ok(true)
        }
    }
}

fn write_text_buffered(path: &Path, text: &str) -> std::io::Result<()> {
    let file = fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(text.as_bytes())?;
    writer.flush()
}

fn resolve_fmt_targets(args: &FmtArgs) -> Result<Vec<FmtTarget>> {
    if let Some(path) = args.path.as_ref()
        && let Some(mode) = infer_kicad_mode(path)
    {
        if !path.exists() {
            anyhow::bail!("File not found: {}", path.display());
        }
        if !path.is_file() {
            anyhow::bail!("Expected a file path, got: {}", path.display());
        }

        return Ok(vec![FmtTarget::Kicad {
            path: path.clone(),
            mode,
        }]);
    }

    let paths: Vec<PathBuf> = args.path.clone().into_iter().collect();
    let zen_files = file_walker::collect_zen_files(&paths)?;

    if zen_files.is_empty() {
        let root_display = if paths.is_empty() {
            let cwd = std::env::current_dir()?;
            cwd.canonicalize().unwrap_or(cwd).display().to_string()
        } else {
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        anyhow::bail!("No .zen files found in {}", root_display);
    }

    Ok(zen_files.into_iter().map(FmtTarget::Zen).collect())
}

fn format_target_file(formatter: &RuffFormatter, target: &FmtTarget, op: FmtOp) -> Result<bool> {
    match target {
        FmtTarget::Zen(path) => format_zen_file(formatter, path, op),
        FmtTarget::Kicad { path, mode } => format_kicad_file(path, op, *mode),
    }
}

fn process_targets(
    formatter: &RuffFormatter,
    targets: &[FmtTarget],
    op: FmtOp,
) -> Result<(Vec<PathBuf>, usize)> {
    let mut files_needing_format = Vec::new();
    let mut failed_count = 0usize;

    for target in targets {
        let path = target.path();
        let file_name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        let spinner = Spinner::builder(format!("{}: {}", file_name, op.spinner_suffix())).start();

        match format_target_file(formatter, target, op) {
            Ok(needs_formatting) => {
                spinner.finish();

                if op.is_check() && needs_formatting {
                    println!(
                        "{} {} (needs formatting)",
                        pcb_ui::icons::warning(),
                        file_name.with_style(Style::Yellow).bold()
                    );
                    files_needing_format.push(path.to_path_buf());
                } else {
                    println!(
                        "{} {}",
                        pcb_ui::icons::success(),
                        file_name.with_style(Style::Green).bold()
                    );
                }
            }
            Err(e) => {
                spinner.error(format!("{file_name}: Format failed"));
                eprintln!("Error: {e}");
                failed_count += 1;
            }
        }
    }

    Ok((files_needing_format, failed_count))
}

pub fn execute(args: FmtArgs) -> Result<()> {
    // Create a ruff formatter instance
    let formatter = RuffFormatter::default();
    let op = FmtOp::from_args(&args);

    // Print version info in debug mode
    debug!("Using ruff formatter");

    let targets = resolve_fmt_targets(&args)?;
    let (files_needing_format, failed_count) = process_targets(&formatter, &targets, op)?;

    // Handle check mode results
    if op.is_check() && (!files_needing_format.is_empty() || failed_count > 0) {
        if !files_needing_format.is_empty() {
            eprintln!("\n{} files need formatting.", files_needing_format.len());
            eprintln!(
                "\nRun 'pcb fmt {}' to format these files.",
                files_needing_format
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }

        if failed_count > 0 {
            eprintln!("\n{} files failed to format.", failed_count);
        }

        anyhow::bail!("Some files are not formatted correctly");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FormatMode, infer_kicad_mode};
    use std::path::Path;

    #[test]
    fn infer_kicad_mode_for_library_table_files() {
        assert_eq!(
            infer_kicad_mode(Path::new("fp-lib-table")),
            Some(FormatMode::LibraryTable)
        );
        assert_eq!(
            infer_kicad_mode(Path::new("sym-lib-table")),
            Some(FormatMode::LibraryTable)
        );
    }

    #[test]
    fn infer_kicad_mode_for_known_kicad_sexpr_extensions() {
        assert_eq!(
            infer_kicad_mode(Path::new("board.kicad_pcb")),
            Some(FormatMode::Normal)
        );
        assert_eq!(
            infer_kicad_mode(Path::new("sheet.kicad_sch")),
            Some(FormatMode::Normal)
        );
        assert_eq!(
            infer_kicad_mode(Path::new("symbol.kicad_sym")),
            Some(FormatMode::Normal)
        );
        assert_eq!(
            infer_kicad_mode(Path::new("footprint.kicad_mod")),
            Some(FormatMode::Normal)
        );
    }

    #[test]
    fn infer_kicad_mode_rejects_non_sexpr_files() {
        assert_eq!(infer_kicad_mode(Path::new("layout.kicad_pro")), None);
        assert_eq!(infer_kicad_mode(Path::new("layout.kicad_prl")), None);
        assert_eq!(infer_kicad_mode(Path::new("main.zen")), None);
    }
}
