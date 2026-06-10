//! Locates the IDF board file (`.emn`) that provides mechanical placements for a board.

use pcb_sch::{ATTR_LAYOUT_NAME, AttributeValue, Schematic};
use pcb_zen_core::config::{Board, IdfConfig};
use pcb_zen_core::resolution::ResolutionResult;
use std::fs;
use std::path::{Path, PathBuf};

/// Returns the path to the board's IDF `.emn` file, if one is configured.
///
/// Resolution order: a `[board.mechanical.idf]` declaration in a matching
/// pcb.toml wins; otherwise the well-known path `mechanical/<board>.emn` next
/// to the board's .zen file or at the workspace root. Ambiguity is an error.
pub fn idf_for_board(
    schematic: &Schematic,
    zen_path: &Path,
    resolution: &ResolutionResult,
) -> anyhow::Result<Option<PathBuf>> {
    let board_name = board_name(schematic, zen_path);
    if let Some(declared) = declared_in_pcb_toml(zen_path, &board_name, resolution)? {
        return Ok(Some(declared));
    }
    at_well_known_path(zen_path, &board_name, resolution)
}

fn declared_in_pcb_toml(
    zen_path: &Path,
    board_name: &str,
    resolution: &ResolutionResult,
) -> anyhow::Result<Option<PathBuf>> {
    let workspace = &resolution.workspace_info;

    let workspace_board = workspace
        .config
        .iter()
        .filter_map(|config| config.board.as_ref())
        .map(|board| (workspace.root.clone(), board));
    let package_boards = workspace.packages.values().filter_map(|package| {
        let board = package.config.board.as_ref()?;
        Some((package.dir(&workspace.root), board))
    });

    let declarations: Vec<(PathBuf, &IdfConfig)> = workspace_board
        .chain(package_boards)
        .filter(|(root, board)| board_matches(board, root, zen_path, board_name))
        .filter_map(|(root, board)| {
            let idf = board.mechanical.as_ref()?.idf.as_ref()?;
            Some((root, idf))
        })
        .collect();

    let Some((root, config)) = at_most_one(declarations, |_| {
        format!(
            "multiple [board.mechanical.idf] configs match {}; keep only one explicit IDF input",
            zen_path.display()
        )
    })?
    else {
        return Ok(None);
    };

    Ok(Some(resolve_declared(&root, config)?))
}

fn resolve_declared(root: &Path, config: &IdfConfig) -> anyhow::Result<PathBuf> {
    let emn = root.join(&config.emn);
    if !emn.exists() {
        anyhow::bail!("declared IDF board file does not exist: {}", emn.display());
    }
    Ok(emn)
}

fn at_well_known_path(
    zen_path: &Path,
    board_name: &str,
    resolution: &ResolutionResult,
) -> anyhow::Result<Option<PathBuf>> {
    let mut candidates = Vec::new();
    if let Some(parent) = zen_path.parent() {
        candidates.push(parent.join("mechanical").join(format!("{board_name}.emn")));
    }
    candidates.push(
        resolution
            .workspace_info
            .root
            .join("mechanical")
            .join(format!("{board_name}.emn")),
    );

    let existing: Vec<PathBuf> = dedup_paths(candidates)
        .into_iter()
        .filter(|path| path.exists())
        .collect();

    at_most_one(existing, |found| {
        format!(
            "multiple convention IDF files match board '{}': {}. Declare [board.mechanical.idf] explicitly.",
            board_name,
            found
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

/// Zero candidates is None, one is the answer, two or more is an error — never a guess.
fn at_most_one<T>(
    mut candidates: Vec<T>,
    ambiguity: impl FnOnce(&[T]) -> String,
) -> anyhow::Result<Option<T>> {
    match candidates.len() {
        0 => Ok(None),
        1 => Ok(Some(candidates.remove(0))),
        _ => anyhow::bail!("{}", ambiguity(&candidates)),
    }
}

fn board_matches(board: &Board, root: &Path, zen_path: &Path, board_name: &str) -> bool {
    if let Some(path) = &board.path {
        same_path(&root.join(path), zen_path)
    } else {
        board.name == board_name
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    let a = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let b = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    a == b
}

fn board_name(schematic: &Schematic, zen_path: &Path) -> String {
    schematic
        .root_ref
        .as_ref()
        .and_then(|root_ref| schematic.instances.get(root_ref))
        .and_then(|root| root.attributes.get(ATTR_LAYOUT_NAME))
        .and_then(AttributeValue::string)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            zen_path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("board")
                .to_owned()
        })
}

pub(crate) fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing: &PathBuf| existing == &path) {
            out.push(path);
        }
    }
    out
}
