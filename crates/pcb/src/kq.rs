//! `pcb kq` cookbook
//!
//! Today `kq` is a projection tool, not a standalone query DSL.
//! The intended query language is `jq` over the emitted JSON.
//!
//! Examples:
//!
//! ```bash
//! # Default semantic symbol projection
//! pcb kq Device.kicad_sym --symbol R
//!
//! # Canonical property/metadata projection
//! pcb kq Device.kicad_sym --view metadata --symbol R
//!
//! # Electrical snapshot for before/after comparison
//! pcb kq Device.kicad_sym --view electrical --symbol R
//!
//! # Low-level raw S-expression tree with tags and spans
//! pcb kq Device.kicad_sym --view raw --symbol R
//!
//! # List all symbols in a library
//! pcb kq Device.kicad_sym --view metadata | jq -r '.symbols[].name'
//!
//! # Show primary metadata for one symbol
//! pcb kq Device.kicad_sym --view metadata --symbol R \
//!   | jq '.symbols[0].metadata.primary'
//!
//! # Show custom properties only
//! pcb kq some_part.kicad_sym --view metadata --symbol SomePart \
//!   | jq '.symbols[0].metadata.custom_properties'
//!
//! # Stable electrical identity for pins
//! pcb kq Device.kicad_sym --view electrical --symbol R \
//!   | jq '.symbols[0].pins[] | {number, signal_name, electrical_type, hidden}'
//!
//! # Compare electrical changes before/after an edit
//! pcb kq before.kicad_sym --view electrical --symbol U1 | jq '.symbols[0].pins' > /tmp/before.json
//! pcb kq after.kicad_sym  --view electrical --symbol U1 | jq '.symbols[0].pins' > /tmp/after.json
//! diff -u /tmp/before.json /tmp/after.json
//!
//! # Inspect raw property nodes
//! pcb kq Device.kicad_sym --view raw --symbol R \
//!   | jq '.node.items[] | select(.tag == "property")'
//!
//! # Inspect all raw pin nodes
//! pcb kq some_part.kicad_sym --view raw --symbol SomePart \
//!   | jq '.. | objects | select(.tag? == "pin")'
//! ```
//!
use anyhow::{Context, Result, bail};
use clap::{Args, ValueEnum};
use pcb_eda::kicad::metadata::SymbolMetadata;
use pcb_eda::kicad::symbol::KicadSymbol;
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use pcb_eda::{Pin, Symbol};
use pcb_sexpr::kicad::symbol::kicad_symbol_lib_items;
use pcb_sexpr::{Sexpr, SexprKind, Span};
use serde::Serialize;
use std::fs;
use std::path::PathBuf;

#[derive(Args, Debug)]
#[command(about = "Inspect KiCad symbol libraries as structured JSON")]
pub struct KqArgs {
    /// KiCad symbol library to inspect
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub path: PathBuf,

    /// Projection to emit
    #[arg(long, value_enum, default_value = "sym")]
    pub view: KqView,

    /// Restrict output to a single top-level symbol
    #[arg(long)]
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum KqView {
    Raw,
    Sym,
    Metadata,
    Electrical,
}

#[derive(Debug, Serialize)]
#[serde(tag = "view", rename_all = "snake_case")]
enum KqOutput {
    Raw {
        path: PathBuf,
        symbol: Option<String>,
        node: RawNode,
    },
    Sym {
        path: PathBuf,
        symbols: Vec<SymbolProjection>,
    },
    Metadata {
        path: PathBuf,
        symbols: Vec<MetadataProjection>,
    },
    Electrical {
        path: PathBuf,
        symbols: Vec<ElectricalProjection>,
    },
}

#[derive(Debug, Serialize)]
struct RawNode {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    span: Span,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    items: Option<Vec<RawNode>>,
}

#[derive(Debug, Serialize)]
struct SymbolProjection {
    name: String,
    extends: Option<String>,
    reference: String,
    footprint: String,
    in_bom: bool,
    datasheet: Option<String>,
    manufacturer: Option<String>,
    mpn: Option<String>,
    description: Option<String>,
    properties: std::collections::HashMap<String, String>,
    pins: Vec<Pin>,
}

#[derive(Debug, Serialize)]
struct MetadataProjection {
    name: String,
    extends: Option<String>,
    metadata: SymbolMetadata,
}

#[derive(Debug, Serialize)]
struct ElectricalProjection {
    name: String,
    extends: Option<String>,
    pins: Vec<ElectricalPin>,
}

#[derive(Debug, Serialize)]
struct ElectricalPin {
    number: String,
    name: String,
    signal_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    electrical_type: Option<String>,
    hidden: bool,
}

pub fn execute(args: KqArgs) -> Result<()> {
    let output = match args.view {
        KqView::Raw => render_raw_output(&args)?,
        KqView::Sym => render_symbol_output(&args)?,
        KqView::Metadata => render_metadata_output(&args)?,
        KqView::Electrical => render_electrical_output(&args)?,
    };

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn render_raw_output(args: &KqArgs) -> Result<KqOutput> {
    let source = fs::read_to_string(&args.path)
        .with_context(|| format!("Failed to read {}", args.path.display()))?;
    let parsed = pcb_sexpr::parse(&source)
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| format!("Failed to parse {}", args.path.display()))?;

    let selected = if let Some(symbol_name) = &args.symbol {
        let root = kicad_symbol_lib_items(&parsed).ok_or_else(|| {
            anyhow::anyhow!("{} is not a KiCad symbol library", args.path.display())
        })?;
        find_top_level_symbol_node(root, symbol_name)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Symbol '{}' not found in {}",
                    symbol_name,
                    args.path.display()
                )
            })?
    } else {
        parsed
    };

    Ok(KqOutput::Raw {
        path: args.path.clone(),
        symbol: args.symbol.clone(),
        node: project_raw_node(&selected),
    })
}

fn render_symbol_output(args: &KqArgs) -> Result<KqOutput> {
    let symbols = load_kicad_symbols(args)?
        .into_iter()
        .map(|kicad_symbol| {
            let extends = kicad_symbol.extends().map(ToOwned::to_owned);
            let symbol: Symbol = kicad_symbol.into();
            SymbolProjection {
                name: symbol.name,
                extends,
                reference: symbol.reference,
                footprint: symbol.footprint,
                in_bom: symbol.in_bom,
                datasheet: symbol.datasheet,
                manufacturer: symbol.manufacturer,
                mpn: symbol.mpn,
                description: symbol.description,
                properties: symbol.properties,
                pins: symbol.pins,
            }
        })
        .collect();

    Ok(KqOutput::Sym {
        path: args.path.clone(),
        symbols,
    })
}

fn render_metadata_output(args: &KqArgs) -> Result<KqOutput> {
    let symbols = load_kicad_symbols(args)?
        .into_iter()
        .map(|kicad_symbol| MetadataProjection {
            name: kicad_symbol.name().to_string(),
            extends: kicad_symbol.extends().map(ToOwned::to_owned),
            metadata: kicad_symbol.metadata(),
        })
        .collect();

    Ok(KqOutput::Metadata {
        path: args.path.clone(),
        symbols,
    })
}

fn render_electrical_output(args: &KqArgs) -> Result<KqOutput> {
    let symbols = load_kicad_symbols(args)?
        .into_iter()
        .map(|kicad_symbol| {
            let extends = kicad_symbol.extends().map(ToOwned::to_owned);
            let symbol: Symbol = kicad_symbol.into();
            ElectricalProjection {
                name: symbol.name,
                extends,
                pins: symbol
                    .pins
                    .into_iter()
                    .map(|pin| ElectricalPin {
                        signal_name: pin.signal_name().to_string(),
                        number: pin.number,
                        name: pin.name,
                        electrical_type: pin.electrical_type,
                        hidden: pin.hidden,
                    })
                    .collect(),
            }
        })
        .collect();

    Ok(KqOutput::Electrical {
        path: args.path.clone(),
        symbols,
    })
}

fn load_kicad_symbols(args: &KqArgs) -> Result<Vec<KicadSymbol>> {
    if args.path.extension().and_then(|ext| ext.to_str()) != Some("kicad_sym") {
        bail!(
            "`pcb kq` semantic views currently support only .kicad_sym files: {}",
            args.path.display()
        );
    }

    let library = KicadSymbolLibrary::from_file(&args.path)
        .with_context(|| format!("Failed to load {}", args.path.display()))?;

    let names = requested_symbol_names(&library, args.symbol.as_deref(), &args.path)?;
    let mut out = Vec::with_capacity(names.len());

    for name in names {
        let kicad_symbol = library
            .get_symbol_lazy(&name)?
            .ok_or_else(|| anyhow::anyhow!("Symbol '{}' not found", name))?;
        out.push(kicad_symbol);
    }

    Ok(out)
}

fn requested_symbol_names(
    library: &KicadSymbolLibrary,
    requested: Option<&str>,
    path: &std::path::Path,
) -> Result<Vec<String>> {
    let mut names: Vec<String> = library
        .symbol_names()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    names.sort();

    if let Some(name) = requested {
        if names.iter().any(|candidate| candidate == name) {
            Ok(vec![name.to_string()])
        } else {
            bail!(
                "Symbol '{}' not found in {}. Available symbols: {}",
                name,
                path.display(),
                names.join(", ")
            );
        }
    } else {
        Ok(names)
    }
}

fn find_top_level_symbol_node<'a>(root: &'a [Sexpr], name: &str) -> Option<&'a Sexpr> {
    root.iter().find(|node| {
        node.as_list().is_some_and(|items| {
            items.first().and_then(Sexpr::as_sym) == Some("symbol")
                && items
                    .get(1)
                    .and_then(|item| item.as_str().or_else(|| item.as_sym()))
                    == Some(name)
        })
    })
}

fn project_raw_node(node: &Sexpr) -> RawNode {
    match &node.kind {
        SexprKind::Symbol(value) => RawNode {
            kind: "symbol",
            tag: None,
            span: node.span,
            value: Some(value.clone()),
            raw: None,
            items: None,
        },
        SexprKind::String(value) => RawNode {
            kind: "string",
            tag: None,
            span: node.span,
            value: Some(value.clone()),
            raw: None,
            items: None,
        },
        SexprKind::Int(value) => RawNode {
            kind: "int",
            tag: None,
            span: node.span,
            value: Some(value.to_string()),
            raw: node.raw_atom.clone(),
            items: None,
        },
        SexprKind::F64(value) => RawNode {
            kind: "float",
            tag: None,
            span: node.span,
            value: Some(value.to_string()),
            raw: node.raw_atom.clone(),
            items: None,
        },
        SexprKind::List(items) => RawNode {
            kind: "list",
            tag: items.first().and_then(Sexpr::as_sym).map(ToOwned::to_owned),
            span: node.span,
            value: None,
            raw: None,
            items: Some(items.iter().map(project_raw_node).collect()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct TempKicadSym {
        _dir: TempDir,
        path: std::path::PathBuf,
    }

    fn temp_kicad_sym(contents: &str) -> TempKicadSym {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.kicad_sym");
        std::fs::write(&path, contents).expect("write symbol lib");
        TempKicadSym { _dir: dir, path }
    }

    #[test]
    fn electrical_projection_uses_signal_name_fallback() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (version 20241209)
  (generator "test")
  (symbol "Base"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "Base" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Datasheet" "" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (symbol "Base_1_1"
      (pin passive line (at 0 0 0) (length 2.54)
        (name "~" (effects (font (size 1.27 1.27))))
        (number "1" (effects (font (size 1.27 1.27)))))
      (pin input line (at 1 2 90) (length 2.54)
        (name "VIN" (effects (font (size 1.27 1.27))))
        (number "2" (effects (font (size 1.27 1.27)))))
    )
  )
)"#,
        );

        let args = KqArgs {
            path: temp.path.clone(),
            view: KqView::Electrical,
            symbol: None,
        };

        let KqOutput::Electrical { symbols, .. } = render_electrical_output(&args).unwrap() else {
            panic!("expected electrical output");
        };

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].pins[0].signal_name, "1");
        assert_eq!(symbols[0].pins[1].signal_name, "VIN");
    }

    #[test]
    fn metadata_projection_includes_inherited_properties() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (version 20241209)
  (generator "test")
  (symbol "Base"
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "Base" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Footprint" "Lib:Pkg" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Datasheet" "https://example.com" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Description" "Base description" (at 0 0 0) (effects (font (size 1.27 1.27))))
  )
  (symbol "Child"
    (extends "Base")
    (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Value" "Child" (at 0 0 0) (effects (font (size 1.27 1.27))))
    (property "Datasheet" "https://child.example.com" (at 0 0 0) (effects (font (size 1.27 1.27))))
  )
)"#,
        );

        let args = KqArgs {
            path: temp.path.clone(),
            view: KqView::Metadata,
            symbol: Some("Child".to_string()),
        };

        let KqOutput::Metadata { symbols, .. } = render_metadata_output(&args).unwrap() else {
            panic!("expected metadata output");
        };

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].extends.as_deref(), Some("Base"));
        assert_eq!(
            symbols[0].metadata.primary.footprint.as_deref(),
            Some("Lib:Pkg")
        );
        assert_eq!(
            symbols[0].metadata.primary.datasheet.as_deref(),
            Some("https://child.example.com")
        );
        assert_eq!(
            symbols[0].metadata.primary.description.as_deref(),
            Some("Base description")
        );
    }

    #[test]
    fn raw_projection_can_select_single_symbol() {
        let temp = temp_kicad_sym(
            r#"(kicad_symbol_lib
  (version 20241209)
  (generator "test")
  (symbol "A" (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27)))))
  (symbol "B" (property "Reference" "R" (at 0 0 0) (effects (font (size 1.27 1.27)))))
)"#,
        );

        let args = KqArgs {
            path: temp.path.clone(),
            view: KqView::Raw,
            symbol: Some("B".to_string()),
        };

        let KqOutput::Raw { node, .. } = render_raw_output(&args).unwrap() else {
            panic!("expected raw output");
        };

        assert_eq!(node.tag.as_deref(), Some("symbol"));
        assert!(!node.span.is_synthetic());
        let items = node.items.expect("raw list items");
        assert_eq!(items[1].value.as_deref(), Some("B"));
    }
}
