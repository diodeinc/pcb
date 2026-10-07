use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use pcb_eda::kicad::{metadata::SymbolMetadata, symbol_library::KicadSymbolLibrary};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(
    after_help = "Examples:\n  pcb inspect ./parts.kicad_sym\n  pcb inspect ./parts.kicad_symdir --format json\n\nJSON contains a symbols array sorted by name. Each entry has name and metadata\n(primary and custom_properties). Inheritance is resolved within the library;\nrelative references and empty property values are preserved. Invalid libraries\nfail without emitting partial JSON. No network or workspace resolution is used."
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
    let library = KicadSymbolLibrary::from_file_strict(&args.path)
        .with_context(|| format!("Cannot inspect {}", args.path.display()))?;
    let symbols = library
        .symbol_names()
        .into_iter()
        .map(|name| {
            let symbol = library
                .get_symbol_lazy(name)
                .with_context(|| {
                    format!("{}: failed to resolve symbol {name:?}", args.path.display())
                })?
                .expect("strict library contains every listed symbol");
            Ok(SymbolInspection {
                name: name.to_owned(),
                metadata: symbol.metadata(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let inspection = Inspection { symbols };

    // Complete validation/resolution before emitting anything, including in JSON mode.
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
