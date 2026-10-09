use anyhow::{Context, Result, anyhow};
use clap::{Args, ValueEnum};
use pcb_eda::kicad::metadata::SymbolMetadata;
use pcb_eda::kicad::symbol_check::unloadable;
use pcb_eda::kicad::symbol_library::{KicadSymbolLibrary, library_paths};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(
    after_help = "Examples:\n  pcb inspect ./parts.kicad_sym\n  pcb inspect ./parts.kicad_symdir --format json\n\nJSON contains a symbols array sorted by name. Each entry has name and metadata\n(primary and custom_properties). Inheritance is resolved within the library;\nrelative references and empty property values are preserved. A library KiCad\ncannot load fails without emitting partial JSON. No network or workspace\nresolution is used."
)]
pub struct InspectArgs {
    /// Local .kicad_sym file or .kicad_symdir library (no workspace required)
    #[arg(value_name = "PATH", value_hint = clap::ValueHint::AnyPath)]
    pub path: PathBuf,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value = "human")]
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Human,
    Json,
}

#[derive(Serialize)]
struct Inspection {
    symbols: Vec<SymbolInspection>,
}

#[derive(Serialize)]
struct SymbolInspection {
    name: String,
    metadata: SymbolMetadata,
}

pub fn execute(args: InspectArgs) -> Result<()> {
    let inspection =
        inspect(&args.path).with_context(|| format!("Cannot inspect {}", args.path.display()))?;

    match args.format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&inspection)?;
            pcb_ui::write_stdout(|stdout| writeln!(stdout, "{json}"))?;
        }
        OutputFormat::Human => pcb_ui::write_stdout(|stdout| {
            if inspection.symbols.is_empty() {
                writeln!(stdout, "No symbols.")?;
            }
            for (index, symbol) in inspection.symbols.iter().enumerate() {
                if index > 0 {
                    writeln!(stdout)?;
                }
                writeln!(stdout, "{}", symbol.name)?;
                for (key, value) in symbol.metadata.to_properties_map() {
                    writeln!(stdout, "  {key}: {value}")?;
                }
            }
            Ok(())
        })?,
    }
    Ok(())
}

/// Load the library the way a build does and refuse what KiCad refuses.
fn inspect(path: &std::path::Path) -> Result<Inspection> {
    let paths = library_paths(path)?;
    let sources = paths
        .iter()
        .map(|path| {
            fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))
        })
        .collect::<Result<_>>()?;
    let library = KicadSymbolLibrary::from_sources(sources)?;
    if let Some(issue) = unloadable(&library).first() {
        let text = &library.sources()[issue.source];
        let line = text[..issue.span.start].matches('\n').count() + 1;
        let column = issue.span.start - text[..issue.span.start].rfind('\n').map_or(0, |at| at + 1);
        return Err(anyhow!(
            "{}:{line}:{}: [{}] {}",
            paths[issue.source].display(),
            column + 1,
            issue.kind,
            issue.message
        ));
    }
    let symbols = library
        .symbol_names()
        .into_iter()
        .map(|name| {
            let symbol = library
                .get_symbol_lazy(name)?
                .ok_or_else(|| anyhow!("symbol {name:?} is listed but not defined"))?;
            Ok(SymbolInspection {
                name: name.to_owned(),
                metadata: symbol.metadata(),
            })
        })
        .collect::<Result<_>>()?;
    Ok(Inspection { symbols })
}
