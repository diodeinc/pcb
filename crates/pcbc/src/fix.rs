use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;
use pcb_ui::prelude::*;
use similar::TextDiff;
use starlark::collections::SmallMap;

use crate::build::{BuildEvalState, select_build_input};

#[derive(Args, Debug, Default, Clone)]
#[command(about = "Fix what `pcb build` reports as fixable")]
pub struct FixArgs {
    /// .zen file(s) or directory to fix. Defaults to current directory.
    ///
    /// When multiple paths are provided, each path must be a .zen file in the same workspace.
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub paths: Vec<PathBuf>,

    /// Show diffs instead of writing files
    #[arg(long)]
    pub diff: bool,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,
}

pub fn execute(args: FixArgs) -> Result<()> {
    let input = select_build_input(&args.paths, false)?;
    let resolution = crate::resolve::resolve(input.resolve_path(), args.offline)?;
    let workspace_root = resolution.workspace_info.root.clone();
    let zen_files = input.collect_zen_files(&resolution.workspace_info)?;
    let state = BuildEvalState::new(resolution);

    // A file that does not evaluate reports no symbols; `pcb build` says why.
    let mut fixed = BTreeMap::new();
    for zen_path in &zen_files {
        let file_name = zen_path.file_name().unwrap().to_string_lossy();
        let spinner = Spinner::builder(format!("{file_name}: Checking")).start();
        if let Some(output) = state.eval(zen_path, SmallMap::new()).output {
            output.fix_symbols(state.file_provider(), &mut fixed);
        }
        spinner.finish();
    }

    for (path, text) in &fixed {
        let shown = path.strip_prefix(&workspace_root).unwrap_or(path).display();
        if args.diff {
            let before = fs::read_to_string(path)
                .with_context(|| format!("Failed to read {}", path.display()))?;
            let diff = TextDiff::from_lines(before.as_str(), text.as_str());
            let (old, new) = (format!("old/{shown}"), format!("new/{shown}"));
            pcb_ui::write_stdout(|stdout| {
                write!(stdout, "{}", diff.unified_diff().header(&old, &new))
            })?;
        } else {
            fs::write(path, text).with_context(|| format!("Failed to write {}", path.display()))?;
            eprintln!("Fixed {shown}");
        }
    }
    if fixed.is_empty() {
        eprintln!("Nothing to fix");
    }
    Ok(())
}
