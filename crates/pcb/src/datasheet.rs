//! `pcb datasheet <QUERY>` — deterministically resolve a component to its datasheet.
//!
//! The query is one of three forms, tried in this order:
//!   1. An encoded component id (base64url JSON `{source, mpn, manufacturer?, backendId}`).
//!   2. A reference designator (e.g. `U3`) — valid only inside a workspace.
//!   3. An MPN.
//!
//! `--refdes`/`--mpn`/`--id` force the interpretation when the heuristic is ambiguous. See
//! [`pcb_diode_api::datasheet_resolve`] for the resolution tiers themselves; this module wires the
//! command up and evaluates the board for reference-designator resolution (reusing the same
//! machinery as `pcb bom`).

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use clap::{Args, ValueEnum};
use pcb_diode_api::datasheet::is_usable_datasheet_value;
use pcb_diode_api::datasheet_resolve::{
    DatasheetSource, Interpretation, MpnResolveConfig, ResolvedDatasheet, datasheet_from_symbol,
    decode_component_id, looks_like_refdes, resolve_component_id, resolve_mpn,
};
use pcb_sch::{InstanceKind, PACKAGE_URI_PREFIX, Schematic};
use pcb_zen::get_workspace_info;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::find_workspace_root;

#[derive(ValueEnum, Debug, Clone, Default)]
pub enum DatasheetFormat {
    /// The resolved datasheet URL or local file path, one line.
    #[default]
    Text,
    /// A JSON object describing the resolution.
    Json,
}

#[derive(Args, Debug)]
#[command(
    about = "Resolve a component to its datasheet",
    long_about = "Deterministically resolve a component to its datasheet and print the result.\n\n\
The QUERY is one of three forms, tried in this order:\n  \
1. An encoded component id (as returned by `pcb search --mode web:components`).\n  \
2. A reference designator (e.g. U3) - valid only inside a workspace.\n  \
3. An MPN.\n\n\
Use --refdes/--mpn/--id to force the interpretation."
)]
pub struct DatasheetArgs {
    /// Encoded component id, reference designator (e.g. U3), or MPN.
    #[arg(value_name = "QUERY")]
    pub query: String,

    /// Force interpreting QUERY as a reference designator.
    #[arg(long, group = "interpretation")]
    pub refdes: bool,

    /// Force interpreting QUERY as an MPN.
    #[arg(long, group = "interpretation")]
    pub mpn: bool,

    /// Force interpreting QUERY as an encoded component id.
    #[arg(long, group = "interpretation")]
    pub id: bool,

    /// Manufacturer to disambiguate parts that share an MPN.
    #[arg(long, value_name = "NAME")]
    pub manufacturer: Option<String>,

    /// Board .zen file for reference-designator resolution (default: discover in workspace).
    #[arg(long, value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub board: Option<PathBuf>,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = DatasheetFormat::Text)]
    pub format: DatasheetFormat,

    /// Disable network access (offline mode) for board evaluation and resolution tiers.
    #[arg(long)]
    pub offline: bool,

    /// Require that pcb.toml and pcb.sum are up-to-date during board evaluation.
    #[arg(long)]
    pub locked: bool,
}

pub fn execute(args: DatasheetArgs) -> Result<()> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let workspace_root = find_workspace_root(&DefaultFileProvider::new(), &cwd).ok();

    let resolved = resolve_query(&args, workspace_root.as_deref())?;

    match args.format {
        DatasheetFormat::Text => println!("{}", resolved.url),
        DatasheetFormat::Json => {
            let obj = serde_json::json!({
                "query": args.query,
                "interpretation": resolved.interpretation,
                "mpn": resolved.mpn,
                "manufacturer": resolved.manufacturer,
                "url": resolved.url,
                "source": resolved.source,
            });
            println!("{}", serde_json::to_string_pretty(&obj)?);
        }
    }

    Ok(())
}

/// Resolve the query into a datasheet, honoring forced-interpretation flags and the heuristic.
fn resolve_query(args: &DatasheetArgs, workspace_root: Option<&Path>) -> Result<ResolvedDatasheet> {
    let query = args.query.trim();
    if query.is_empty() {
        bail!("empty query");
    }

    // --- Forced interpretations ---
    if args.id {
        let token = pcb_diode_api::auth::get_valid_token()?;
        return resolve_component_id(&token, query);
    }
    if args.refdes {
        let root = workspace_root.ok_or_else(|| {
            anyhow!("reference designators can only be resolved inside a workspace")
        })?;
        return resolve_refdes(args, root, query, true)?
            .ok_or_else(|| anyhow!("reference designator '{query}' not found on the board"));
    }
    if args.mpn {
        return resolve_as_mpn(args, workspace_root, query);
    }

    // --- Heuristic (in order) ---
    // 1. Encoded component id.
    if decode_component_id(query).is_some() {
        let token = pcb_diode_api::auth::get_valid_token()?;
        return resolve_component_id(&token, query);
    }

    // 2. Reference designator: only when refdes-shaped, inside a workspace, and it actually
    //    matches a BOM designator. Board discovery/eval failures fall through to MPN.
    if looks_like_refdes(query)
        && let Some(root) = workspace_root
        && let Some(resolved) = resolve_refdes(args, root, query, false)?
    {
        return Ok(resolved);
    }

    // 3. MPN.
    resolve_as_mpn(args, workspace_root, query)
}

fn resolve_as_mpn(
    args: &DatasheetArgs,
    workspace_root: Option<&Path>,
    mpn: &str,
) -> Result<ResolvedDatasheet> {
    let cfg = MpnResolveConfig {
        workspace_root,
        manufacturer: args.manufacturer.as_deref(),
        offline: args.offline,
    };
    resolve_mpn(mpn, &cfg)
}

/// Component fields extracted from a resolved schematic instance.
struct RefdesComponent {
    symbol_path: Option<String>,
    symbol_name: Option<String>,
    mpn: Option<String>,
    manufacturer: Option<String>,
    datasheet: Option<String>,
}

/// Resolve a reference designator by evaluating the board.
///
/// Prefers the design's own resolved symbol (its `.kicad_sym` `Datasheet` property and a sibling
/// `<MPN>.pdf`), then the component's datasheet attribute, then falls back to the MPN tiers.
///
/// Returns `Ok(None)` only when the designator does not match a component *and* `required` is
/// false (auto heuristic), so the caller can fall through to MPN resolution. When `required` is
/// false, board discovery/evaluation errors are also swallowed into `Ok(None)`.
fn resolve_refdes(
    args: &DatasheetArgs,
    workspace_root: &Path,
    refdes: &str,
    required: bool,
) -> Result<Option<ResolvedDatasheet>> {
    let board_zen = match discover_board(args, workspace_root) {
        Ok(b) => b,
        Err(e) => return if required { Err(e) } else { Ok(None) },
    };

    let schematic = match eval_board(&board_zen, args.offline, args.locked) {
        Ok(s) => s,
        Err(e) => return if required { Err(e) } else { Ok(None) },
    };

    let Some(component) = find_component_by_refdes(&schematic, refdes) else {
        return Ok(None);
    };

    // We have matched a real component: from here on, failures are hard errors even in auto mode.
    let board_dir = board_zen.parent().unwrap_or_else(|| Path::new("."));

    // 1. The design's own resolved symbol (exact design intent).
    if let Some(symbol_ref) = component.symbol_path.as_deref() {
        let symbol_path = resolve_symbol_path(symbol_ref, &schematic);
        if let Some(url) = datasheet_from_symbol(
            &symbol_path,
            component.mpn.as_deref(),
            component.symbol_name.as_deref(),
        ) {
            return Ok(Some(ResolvedDatasheet {
                interpretation: Interpretation::Refdes,
                mpn: component.mpn.clone(),
                manufacturer: component.manufacturer.clone(),
                url,
                source: DatasheetSource::Workspace,
            }));
        }
    }

    // 2. The datasheet attribute recorded directly on the component.
    if let Some(ds) = component.datasheet.as_deref()
        && let Some(url) = normalize_design_datasheet(ds, board_dir)
    {
        return Ok(Some(ResolvedDatasheet {
            interpretation: Interpretation::Refdes,
            mpn: component.mpn.clone(),
            manufacturer: component.manufacturer.clone(),
            url,
            source: DatasheetSource::Workspace,
        }));
    }

    // 3. Fall back to the MPN tier chain.
    if let Some(mpn) = component.mpn.as_deref() {
        let cfg = MpnResolveConfig {
            workspace_root: Some(workspace_root),
            manufacturer: component
                .manufacturer
                .as_deref()
                .or(args.manufacturer.as_deref()),
            offline: args.offline,
        };
        let mut resolved = resolve_mpn(mpn, &cfg)?;
        resolved.interpretation = Interpretation::Refdes;
        if resolved.manufacturer.is_none() {
            resolved.manufacturer = component.manufacturer.clone();
        }
        return Ok(Some(resolved));
    }

    bail!("component '{refdes}' found but no datasheet on record");
}

/// Discover the board .zen file for reference-designator resolution.
fn discover_board(args: &DatasheetArgs, workspace_root: &Path) -> Result<PathBuf> {
    if let Some(board) = &args.board {
        crate::file_walker::require_zen_file(board)?;
        return Ok(board.clone());
    }

    let info = get_workspace_info(&DefaultFileProvider::new(), workspace_root)?;
    let boards = info.boards();
    match boards.len() {
        0 => bail!("no board found in workspace; pass --board <file>"),
        1 => {
            let board = boards.values().next().unwrap();
            Ok(board.absolute_zen_path(&info.root))
        }
        _ => {
            let names: Vec<_> = boards.keys().cloned().collect();
            bail!(
                "multiple boards found ({}); pass --board <file>",
                names.join(", ")
            )
        }
    }
}

/// Evaluate a board .zen and produce its schematic (same machinery as `pcb bom`).
fn eval_board(board_zen: &Path, offline: bool, locked: bool) -> Result<Schematic> {
    let resolution = crate::resolve::resolve(Some(board_zen), offline, locked)?;
    let eval_result = pcb_zen::eval(board_zen, resolution);
    let output = eval_result.output_result().map_err(|_| {
        anyhow!(
            "failed to build {} - cannot resolve reference designator",
            board_zen.display()
        )
    })?;
    output.to_schematic()
}

fn find_component_by_refdes(schematic: &Schematic, refdes: &str) -> Option<RefdesComponent> {
    schematic
        .instances
        .values()
        .find(|inst| {
            inst.kind == InstanceKind::Component
                && inst.reference_designator.as_deref() == Some(refdes)
        })
        .map(|inst| RefdesComponent {
            symbol_path: inst.string_attr(&["symbol_path"]),
            symbol_name: inst.string_attr(&["symbol_name"]),
            mpn: inst.mpn(),
            manufacturer: inst.manufacturer(),
            datasheet: inst.string_attr(&["datasheet", "Datasheet"]),
        })
}

/// Resolve a symbol reference (absolute path or `package://` URI) to a filesystem path.
fn resolve_symbol_path(symbol_ref: &str, schematic: &Schematic) -> PathBuf {
    if symbol_ref.starts_with(PACKAGE_URI_PREFIX)
        && let Ok(path) = schematic.resolve_package_uri(symbol_ref)
    {
        return path;
    }
    PathBuf::from(symbol_ref)
}

/// Normalize a component's `datasheet` attribute to a usable URL or existing local path.
fn normalize_design_datasheet(datasheet: &str, board_dir: &Path) -> Option<String> {
    let trimmed = datasheet.trim();
    if is_usable_datasheet_value(trimmed) {
        return Some(trimmed.to_string());
    }

    // Treat as a filesystem path, relative to the board directory when not absolute.
    let path = Path::new(trimmed);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        board_dir.join(path)
    };
    resolved
        .is_file()
        .then(|| resolved.to_string_lossy().into_owned())
}
