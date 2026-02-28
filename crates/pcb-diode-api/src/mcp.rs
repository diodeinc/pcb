use anyhow::{Context, Result, anyhow, bail};
use json_patch::merge;
use once_cell::sync::Lazy;
use pcb_eda::kicad::metadata::SymbolMetadata;
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use pcb_mcp::{CallToolResult, McpContext, ToolInfo};
use pcb_sexpr::Sexpr;
use pcb_sexpr::formatter::{FormatMode, format_tree};
use pcb_sexpr::kicad::symbol::{
    find_symbol, find_symbol_index, kicad_symbol_lib_items, kicad_symbol_lib_items_mut,
    rewrite_symbol_properties, symbol_declares_extends, symbol_names, symbol_properties,
};
use pcb_zen::cache_index::{cache_base, ensure_workspace_cache_symlink};
use pcb_zen::ensure_sparse_checkout;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::find_workspace_root;
use rayon::prelude::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// JSON Schema for Availability - single source of truth
static AVAILABILITY_SCHEMA: Lazy<Value> = Lazy::new(|| {
    json!({
        "type": ["object", "null"],
        "description": "Pricing and availability data",
        "properties": {
            "us": {
                "type": ["object", "null"],
                "description": "Best US availability",
                "properties": {
                    "price": {"type": ["number", "null"], "description": "Unit price at qty=1"},
                    "stock": {"type": "integer", "description": "Stock from best offer"},
                    "alt_stock": {"type": "integer", "description": "Combined stock from other offers"}
                }
            },
            "global": {
                "type": ["object", "null"],
                "description": "Best global availability",
                "properties": {
                    "price": {"type": ["number", "null"], "description": "Unit price at qty=1"},
                    "stock": {"type": "integer", "description": "Stock from best offer"},
                    "alt_stock": {"type": "integer", "description": "Combined stock from other offers"}
                }
            },
            "offers": {
                "type": "array",
                "description": "Raw distributor offers",
                "items": {
                    "type": "object",
                    "properties": {
                        "region": {"type": "string"},
                        "distributor": {"type": "string"},
                        "stock": {"type": "integer"},
                        "price": {"type": ["number", "null"]},
                        "part_id": {"type": ["string", "null"]}
                    }
                }
            }
        }
    })
});

const KICAD_SYMBOL_CUSTOM_PROPERTIES_DESCRIPTION: &str = "Arbitrary non-canonical KiCad properties. Canonical/reserved keys (`Reference`, `Value`, \
     `Footprint`, `Datasheet`, `Description`, `ki_keywords`, `ki_fp_filters`, `ki_description`) \
     are represented via `primary`, not here.";

static KICAD_SYMBOL_PRIMARY_METADATA_SCHEMA: Lazy<Value> = Lazy::new(|| {
    json!({
        "type": "object",
        "description": "Canonical KiCad metadata fields normalized to structured keys.",
        "properties": {
            "reference": {"type": ["string", "null"], "description": "Mapped to/from KiCad `Reference`."},
            "value": {"type": ["string", "null"], "description": "Mapped to/from KiCad `Value`."},
            "footprint": {"type": ["string", "null"], "description": "Mapped to/from KiCad `Footprint`."},
            "datasheet": {"type": ["string", "null"], "description": "Mapped to/from KiCad `Datasheet`."},
            "description": {"type": ["string", "null"], "description": "Mapped to/from KiCad `Description`. Legacy `ki_description` is normalized here when `Description` is absent."},
            "keywords": {"type": ["array", "null"], "items": {"type": "string"}, "description": "Mapped to/from KiCad `ki_keywords` (stored as a single space-separated string in .kicad_sym)."},
            "footprint_filters": {"type": ["array", "null"], "items": {"type": "string"}, "description": "Mapped to/from KiCad `ki_fp_filters` (stored as a single space-separated string in .kicad_sym)."}
        }
    })
});

static KICAD_SYMBOL_METADATA_SCHEMA: Lazy<Value> = Lazy::new(|| {
    json!({
        "type": "object",
        "properties": {
            "primary": KICAD_SYMBOL_PRIMARY_METADATA_SCHEMA.clone(),
            "custom_properties": {
                "type": "object",
                "description": KICAD_SYMBOL_CUSTOM_PROPERTIES_DESCRIPTION,
                "additionalProperties": {"type": "string"}
            }
        },
        "required": ["primary", "custom_properties"]
    })
});

static KICAD_SYMBOL_METADATA_CHANGES_SCHEMA: Lazy<Value> = Lazy::new(|| {
    json!({
        "type": "object",
        "properties": {
            "primary_set": {"type": "array", "items": {"type": "string"}},
            "primary_cleared": {"type": "array", "items": {"type": "string"}},
            "custom_set": {"type": "array", "items": {"type": "string"}},
            "custom_removed": {"type": "array", "items": {"type": "string"}}
        },
        "required": ["primary_set", "primary_cleared", "custom_set", "custom_removed"]
    })
});

fn kicad_symbol_metadata_schema(description: Option<&str>) -> Value {
    let mut schema = KICAD_SYMBOL_METADATA_SCHEMA.clone();

    if let Some(description) = description {
        schema
            .as_object_mut()
            .expect("schema must be object")
            .insert("description".to_string(), json!(description));
    }

    schema
}

fn kicad_symbol_metadata_mutation_output_schema(operation: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "kicad_sym_path": {"type": "string"},
            "symbol_name": {"type": "string"},
            "operation": {"type": "string", "enum": [operation]},
            "dry_run": {"type": "boolean"},
            "applied": {"type": "boolean"},
            "changed": {"type": "boolean"},
            "changes": KICAD_SYMBOL_METADATA_CHANGES_SCHEMA.clone(),
            "metadata": kicad_symbol_metadata_schema(None)
        },
        "required": ["kicad_sym_path", "symbol_name", "operation", "dry_run", "applied", "changed", "changes", "metadata"]
    })
}

/// Registry search result - shared between MCP and CLI JSON output
#[derive(Debug, Clone, Serialize)]
pub struct RegistrySearchResult {
    #[serde(flatten)]
    pub part: crate::RegistryPart,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<pcb_sch::bom::Availability>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_path: Option<String>,
}

fn required_str(args: Option<&Value>, key: &str) -> Result<String> {
    args.and_then(|a| a.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| anyhow::anyhow!("{} required", key))
}

pub fn tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "search_registry",
            description: "Search the Zener package registry for reference designs, modules, and components. Always try this FIRST when adding components to a board. Prefer modules and reference designs over raw components - they include complete implementations. Returns package URLs for use in Module() - dependencies auto-added to pcb.toml. Each result includes cache_path where package source is checked out locally, and pricing and availability data (stock levels, unit prices, distributor offers) for components with MPN. Only use search_component/add_component if nothing found here.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Search query - can be MPN, description, or keywords (e.g., 'buck converter', 'STM32', 'USB-C')"
                    }
                },
                "required": ["query"]
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "url": {"type": "string", "description": "Package URL for use in load() or Module(). Run `pcb doc --package <url>@<version>` to view docs."},
                                "name": {"type": "string"},
                                "category": {"type": ["string", "null"], "description": "Package type: component, module, or reference"},
                                "part_type": {"type": ["string", "null"], "description": "Component type (e.g., 'voltage regulator', 'MOSFET', 'connector')"},
                                "mpn": {"type": ["string", "null"]},
                                "manufacturer": {"type": ["string", "null"]},
                                "description": {"type": ["string", "null"]},
                                "version": {"type": ["string", "null"]},
                                "dependencies": {"type": "array", "items": {"type": "string"}, "description": "Package URLs this depends on"},
                                "dependents": {"type": "array", "items": {"type": "string"}, "description": "Package URLs that use this"},
                                "cache_path": {"type": ["string", "null"], "description": "Local path where package source is checked out."},
                                "availability": {"$ref": "#/$defs/Availability"}
                            },
                            "required": ["url", "name"]
                        }
                    }
                },
                "$defs": {"Availability": AVAILABILITY_SCHEMA.clone()},
                "required": ["results"]
            })),
        },
        ToolInfo {
            name: "search_component",
            description: "Search Diode's online component database to find components to add to your workspace. IMPORTANT: Only use this AFTER trying search_registry first - registry packages are preferred because they're complete and tested. Use this tool only when: (1) search_registry found no suitable package, or (2) you need a specific part number not in the registry. Returns component_id for use with add_component, plus pricing and availability data (stock levels, unit prices, distributor offers) for each result.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "part_number": {
                        "type": "string",
                        "description": "Part number or search query"
                    }
                },
                "required": ["part_number"]
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "results": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "component_id": {"type": "string"},
                                "part_number": {"type": "string"},
                                "manufacturer": {"type": ["string", "null"]},
                                "description": {"type": ["string", "null"]},
                                "package_category": {"type": ["string", "null"]},
                                "model_availability": {
                                    "type": "object",
                                    "properties": {
                                        "ecad_model": {"type": "boolean"},
                                        "step_model": {"type": "boolean"}
                                    }
                                },
                                "availability": {"$ref": "#/$defs/Availability"}
                            },
                            "required": ["component_id", "part_number"]
                        }
                    }
                },
                "$defs": {"Availability": AVAILABILITY_SCHEMA.clone()},
                "required": ["results"]
            })),
        },
        ToolInfo {
            name: "add_component",
            description: "Download a component from Diode's online database and add it to your workspace at ./components/<MFR>/<PART>/<PART>.zen. Requires component_id and part_number from search_component results. Downloads symbol, footprint, 3D model, and datasheet. NOTE: Prefer using packages from search_registry when available - they include complete, tested implementations.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "component_id": {
                        "type": "string",
                        "description": "Component ID from search_component results"
                    },
                    "part_number": {
                        "type": "string",
                        "description": "Part number from search_component results"
                    },
                    "manufacturer": {
                        "type": "string",
                        "description": "Manufacturer name from search_component results"
                    }
                },
                "required": ["component_id", "part_number"]
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the created .zen file"}
                },
                "required": ["path"]
            })),
        },
        ToolInfo {
            name: "resolve_datasheet",
            description: "Resolve a datasheet into local Markdown + image assets for downstream reading. Use this tool when datasheet content is needed and no local Markdown datasheet is already available. Accepts exactly one of: datasheet URL, local PDF path, or local .kicad_sym path (reads Datasheet property). For .kicad_sym libraries containing multiple symbols, provide symbol_name. URL inputs are sent directly to scan/process via sourceUrl (server fetches the PDF). Returns local filesystem paths to a Markdown datasheet file and an images directory referenced by that Markdown. Prefer this tool over ad-hoc downloads or direct PDF parsing.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "datasheet_url": {
                        "type": "string",
                        "description": "Datasheet URL (http/https)"
                    },
                    "pdf_path": {
                        "type": "string",
                        "description": "Path to a local .pdf datasheet"
                    },
                    "kicad_sym_path": {
                        "type": "string",
                        "description": "Path to a local .kicad_sym file containing a Datasheet property"
                    },
                    "symbol_name": {
                        "type": "string",
                        "description": "Symbol name to use when kicad_sym_path points to a library with multiple symbols"
                    }
                },
                "additionalProperties": false,
                "dependentRequired": {
                    "symbol_name": ["kicad_sym_path"]
                }
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "markdown_path": {"type": "string"},
                    "images_dir": {"type": "string"},
                    "pdf_path": {"type": "string"},
                    "datasheet_url": {"type": ["string", "null"]}
                },
                "required": ["markdown_path", "images_dir", "pdf_path"]
            })),
        },
        ToolInfo {
            name: "read_kicad_symbol_metadata",
            description: "Read metadata from a symbol in a KiCad .kicad_sym library and return it as structured JSON. Canonical KiCad properties are normalized under `metadata.primary`: `Reference` -> `reference`, `Value` -> `value`, `Footprint` -> `footprint`, `Datasheet` -> `datasheet`, `Description` -> `description`, `ki_keywords` -> `keywords` (space-separated in KiCad), and `ki_fp_filters` -> `footprint_filters` (space-separated in KiCad). Legacy KiCad `ki_description` is treated as an alias of `Description` and normalized into `primary.description`. All other non-canonical properties are returned under `metadata.custom_properties`. Use this tool when you need a reliable, programmatic view of symbol metadata before editing. If `resolve_extends` is true, inherited properties from an `extends` chain are merged; if false, only properties directly declared on the target symbol are returned.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kicad_sym_path": {
                        "type": "string",
                        "description": "Path to the source .kicad_sym file"
                    },
                    "symbol_name": {
                        "type": "string",
                        "description": "Name of the symbol to read. Optional only when the library contains exactly one symbol."
                    },
                    "resolve_extends": {
                        "type": "boolean",
                        "description": "When true (default), include inherited properties from parent symbols via `extends`."
                    },
                    "include_raw_properties": {
                        "type": "boolean",
                        "description": "When true, include the raw key/value property map alongside structured metadata for auditing/debugging."
                    }
                },
                "required": ["kicad_sym_path"]
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "kicad_sym_path": {"type": "string"},
                    "symbol_name": {"type": "string"},
                    "resolve_extends": {"type": "boolean"},
                    "metadata": kicad_symbol_metadata_schema(None),
                    "raw_properties": {
                        "type": "object",
                        "additionalProperties": {"type": "string"}
                    },
                    "warnings": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["kicad_sym_path", "symbol_name", "resolve_extends", "metadata"]
            })),
        },
        ToolInfo {
            name: "write_kicad_symbol_metadata",
            description: "Write metadata for a symbol in a KiCad .kicad_sym library using strict full-write semantics. The provided `metadata` object becomes the complete metadata state for the symbol: canonical KiCad properties are regenerated from `metadata.primary` (`keywords` -> `ki_keywords`, `footprint_filters` -> `ki_fp_filters`, both serialized as space-separated strings), and non-canonical properties are regenerated from `metadata.custom_properties`. `primary.description` writes canonical `Description`; legacy `ki_description` is not written separately. Any existing metadata not present in the input is removed. Use `dry_run` to preview changes without modifying files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kicad_sym_path": {
                        "type": "string",
                        "description": "Path to the target .kicad_sym file"
                    },
                    "symbol_name": {
                        "type": "string",
                        "description": "Name of the symbol to update"
                    },
                    "metadata": kicad_symbol_metadata_schema(Some("Full structured metadata to write.")),
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true, compute and return changes without writing to disk."
                    }
                },
                "required": ["kicad_sym_path", "symbol_name", "metadata"]
            }),
            output_schema: Some(kicad_symbol_metadata_mutation_output_schema("write")),
        },
        ToolInfo {
            name: "merge_kicad_symbol_metadata",
            description: "Apply RFC 7396 JSON Merge Patch to structured symbol metadata in a KiCad .kicad_sym library. This is the standards-based incremental update tool: object members in `metadata_patch` update existing metadata, and members set to `null` are deleted. Arrays are replaced as whole values per RFC 7396. Canonical KiCad keys are patched via `metadata_patch.primary` (for example `primary.keywords` maps to KiCad `ki_keywords`; `primary.footprint_filters` maps to `ki_fp_filters`; `primary.description` maps to canonical `Description`, and legacy `ki_description` is normalized into that field on read). Use `custom_properties` only for non-canonical properties; canonical/reserved keys are rejected there. After patching, the resulting metadata is validated and written back to KiCad symbol properties. Use `dry_run` to preview changes without modifying files.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kicad_sym_path": {
                        "type": "string",
                        "description": "Path to the target .kicad_sym file"
                    },
                    "symbol_name": {
                        "type": "string",
                        "description": "Name of the symbol to update"
                    },
                    "metadata_patch": {
                        "type": "object",
                        "description": "RFC 7396 merge patch applied to structured metadata"
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true, compute and return changes without writing to disk."
                    }
                },
                "required": ["kicad_sym_path", "symbol_name", "metadata_patch"]
            }),
            output_schema: Some(kicad_symbol_metadata_mutation_output_schema("merge_patch")),
        },
    ]
}

pub fn handle(name: &str, args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    match name {
        "search_registry" => search_registry(args, ctx),
        "search_component" => search_component(args, ctx),
        "add_component" => add_component(args, ctx),
        "resolve_datasheet" => resolve_datasheet(args, ctx),
        "read_kicad_symbol_metadata" => read_kicad_symbol_metadata(args, ctx),
        "write_kicad_symbol_metadata" => write_kicad_symbol_metadata(args, ctx),
        "merge_kicad_symbol_metadata" => merge_kicad_symbol_metadata(args, ctx),
        _ => bail!("Unknown tool: {}", name),
    }
}

fn search_registry(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let query = required_str(args.as_ref(), "query")?;

    ctx.log("info", &format!("Searching registry for: {}", query));
    let client = crate::RegistryClient::open()?;
    // Use search_filtered with RRF merging (same as TUI) for consistent results
    let results = client.search_filtered(&query, 10, None)?;
    ctx.log("info", &format!("Found {} results", results.len()));

    // Detect workspace and ensure cache symlink if present
    let workspace_cache = std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            let fp = DefaultFileProvider::new();
            find_workspace_root(&fp, &cwd).ok()
        })
        .and_then(|ws_root| {
            ensure_workspace_cache_symlink(&ws_root).ok()?;
            Some(ws_root.join(".pcb/cache"))
        });

    // Ensure packages are checked out in parallel
    let cache = cache_base();
    let cache_paths: Vec<Option<PathBuf>> = results
        .par_iter()
        .map(|r| {
            let version = r.version.as_deref()?;
            let checkout_dir = cache.join(&r.url).join(version);
            match ensure_sparse_checkout(&checkout_dir, &r.url, version, true, None) {
                Ok(path) => {
                    // If in workspace, remap to workspace-relative path
                    if let Some(ref ws_cache) = workspace_cache
                        && let Ok(relative) = path.strip_prefix(&cache)
                    {
                        let ws_path = ws_cache.join(relative);
                        if ws_path.exists() {
                            return Some(ws_path);
                        }
                    }
                    Some(path)
                }
                Err(e) => {
                    log::warn!("Failed to checkout {}@{}: {}", r.url, version, e);
                    None
                }
            }
        })
        .collect();

    // Fetch availability for results that have MPN
    let availability_map = crate::bom::fetch_availability_for_results(&results);

    let formatted: Vec<_> = results
        .iter()
        .enumerate()
        .zip(cache_paths.iter())
        .map(|((idx, r), cache_path)| {
            let dependencies: Vec<_> = client
                .get_dependencies(r.id)
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.url)
                .collect();
            let dependents: Vec<_> = client
                .get_dependents(r.id)
                .unwrap_or_default()
                .into_iter()
                .map(|d| d.url)
                .collect();

            RegistrySearchResult {
                part: r.clone(),
                dependencies,
                dependents,
                cache_path: cache_path.as_ref().map(|p| p.display().to_string()),
                availability: availability_map.get(&idx).cloned(),
            }
        })
        .collect();

    Ok(CallToolResult::json(&json!({"results": formatted})))
}

fn search_component(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let part_number = required_str(args.as_ref(), "part_number")?;

    ctx.log("info", &format!("Searching for component: {}", part_number));
    let token = crate::auth::get_valid_token()?;
    let results = crate::search_components_with_availability(&token, &part_number)?;
    ctx.log("info", &format!("Found {} results", results.len()));

    Ok(CallToolResult::json(&json!({"results": results})))
}

fn add_component(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let component_id = required_str(args.as_ref(), "component_id")?;
    let part_number = required_str(args.as_ref(), "part_number")?;

    ctx.log("info", "Authenticating...");
    let token = crate::auth::get_valid_token()?;

    ctx.log("info", &format!("Adding component: {}", part_number));

    let workspace = std::env::current_dir()?;
    let manufacturer = args
        .as_ref()
        .and_then(|a| a.get("manufacturer"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());
    ctx.progress(2, 2, "Adding to workspace");
    let result = crate::add_component_to_workspace(
        &token,
        &component_id,
        &part_number,
        &workspace,
        manufacturer.as_deref(),
        None, // Use default scan model
    )?;

    ctx.log(
        "info",
        &format!("Component added to {}", result.component_path.display()),
    );

    Ok(CallToolResult::json(&json!({
        "path": result.component_path.display().to_string()
    })))
}

fn resolve_datasheet(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let input = crate::datasheet::parse_resolve_request(args.as_ref())?;
    ctx.log("info", "Authenticating...");
    let token = crate::auth::get_valid_token()?;
    ctx.log("info", "Resolving datasheet...");
    let response = crate::datasheet::resolve_datasheet(&token, &input)?;
    ctx.log(
        "info",
        &format!("Created markdown at {}", response.markdown_path),
    );

    Ok(CallToolResult::json(&serde_json::to_value(response)?))
}

#[derive(Debug, Default, Serialize)]
struct MetadataChanges {
    primary_set: Vec<String>,
    primary_cleared: Vec<String>,
    custom_set: Vec<String>,
    custom_removed: Vec<String>,
}

impl MetadataChanges {
    fn changed(&self) -> bool {
        !(self.primary_set.is_empty()
            && self.primary_cleared.is_empty()
            && self.custom_set.is_empty()
            && self.custom_removed.is_empty())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadKicadSymbolMetadataArgs {
    kicad_sym_path: String,
    symbol_name: Option<String>,
    #[serde(default = "default_true")]
    resolve_extends: bool,
    #[serde(default)]
    include_raw_properties: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteKicadSymbolMetadataArgs {
    kicad_sym_path: String,
    symbol_name: String,
    metadata: SymbolMetadata,
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MergeKicadSymbolMetadataArgs {
    kicad_sym_path: String,
    symbol_name: String,
    metadata_patch: Value,
    #[serde(default)]
    dry_run: bool,
}

fn read_kicad_symbol_metadata(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let params = parse_args::<ReadKicadSymbolMetadataArgs>(args, "read_kicad_symbol_metadata")?;
    let path = PathBuf::from(&params.kicad_sym_path);
    let source =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;

    let library = KicadSymbolLibrary::from_string_lazy(source.as_str())
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    let available_symbols: Vec<String> = library
        .symbol_names()
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
    let selected_symbol =
        select_symbol_name(params.symbol_name.as_deref(), &available_symbols, &path)?;

    let mut warnings = Vec::new();
    let (metadata, raw_properties): (SymbolMetadata, BTreeMap<String, String>) = if params
        .resolve_extends
    {
        let symbol = library
            .get_symbol_lazy(&selected_symbol)
            .with_context(|| format!("Failed to parse symbol '{}'", selected_symbol))?
            .ok_or_else(|| anyhow!("Symbol '{}' not found", selected_symbol))?;
        if symbol.extends().is_some() {
            warnings.push(
                "resolve_extends=true: returned metadata includes properties inherited via extends"
                    .to_string(),
            );
        }
        (
            symbol.metadata(),
            symbol
                .properties()
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        )
    } else {
        let parsed = parse_kicad_symbol_source(&source, &path)?;
        let root = kicad_symbol_lib_items(&parsed)
            .ok_or_else(|| anyhow!("Invalid KiCad root structure"))?;
        let symbol_list = symbol_or_error(root, &selected_symbol, &path)?;
        if symbol_declares_extends(symbol_list) {
            warnings.push(
                "resolve_extends=false: returned metadata excludes inherited properties from extends"
                    .to_string(),
            );
        }
        let direct_properties = symbol_properties(symbol_list);
        (
            SymbolMetadata::from_property_iter(
                direct_properties
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone())),
            ),
            direct_properties,
        )
    };

    let mut response = serde_json::Map::new();
    response.insert("kicad_sym_path".to_string(), json!(params.kicad_sym_path));
    response.insert("symbol_name".to_string(), json!(selected_symbol));
    response.insert("resolve_extends".to_string(), json!(params.resolve_extends));
    response.insert("metadata".to_string(), serde_json::to_value(&metadata)?);
    if params.include_raw_properties {
        response.insert(
            "raw_properties".to_string(),
            serde_json::to_value(raw_properties)?,
        );
    }
    if !warnings.is_empty() {
        response.insert("warnings".to_string(), json!(warnings));
    }

    ctx.log("info", "Read KiCad symbol metadata");
    Ok(CallToolResult::json(&Value::Object(response)))
}

fn write_kicad_symbol_metadata(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let params = parse_args::<WriteKicadSymbolMetadataArgs>(args, "write_kicad_symbol_metadata")?;
    let loaded = load_symbol_for_update(&params.kicad_sym_path, &params.symbol_name)?;
    apply_loaded_metadata_update(loaded, params.metadata, params.dry_run, "write", ctx)
}

fn merge_kicad_symbol_metadata(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let params = parse_args::<MergeKicadSymbolMetadataArgs>(args, "merge_kicad_symbol_metadata")?;
    if !params.metadata_patch.is_object() {
        bail!("metadata_patch must be an object");
    }

    let loaded = load_symbol_for_update(&params.kicad_sym_path, &params.symbol_name)?;

    let mut merged_value = serde_json::to_value(loaded.current_metadata.clone())?;
    merge(&mut merged_value, &params.metadata_patch);
    let next_metadata: SymbolMetadata = serde_json::from_value(merged_value)
        .map_err(|e| anyhow!("metadata_patch produced invalid metadata: {}", e))?;
    apply_loaded_metadata_update(loaded, next_metadata, params.dry_run, "merge_patch", ctx)
}

struct LoadedSymbolForUpdate {
    kicad_sym_path: String,
    path: PathBuf,
    parsed: Sexpr,
    symbol_name: String,
    symbol_idx: usize,
    current_properties: BTreeMap<String, String>,
    current_metadata: SymbolMetadata,
}

fn load_symbol_for_update(
    kicad_sym_path: &str,
    symbol_name: &str,
) -> Result<LoadedSymbolForUpdate> {
    let path = PathBuf::from(kicad_sym_path);
    let source =
        fs::read_to_string(&path).with_context(|| format!("Failed to read {}", path.display()))?;
    let parsed = parse_kicad_symbol_source(&source, &path)?;
    let root_items =
        kicad_symbol_lib_items(&parsed).ok_or_else(|| anyhow!("Invalid KiCad root structure"))?;
    let symbol_idx = symbol_index_or_error(root_items, symbol_name, &path)?;
    let symbol_items = root_items
        .get(symbol_idx)
        .and_then(Sexpr::as_list)
        .ok_or_else(|| anyhow!("Invalid symbol structure for '{}'", symbol_name))?;

    let current_properties = symbol_properties(symbol_items);
    let current_metadata = SymbolMetadata::from_property_iter(
        current_properties
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );

    Ok(LoadedSymbolForUpdate {
        kicad_sym_path: kicad_sym_path.to_string(),
        path,
        parsed,
        symbol_name: symbol_name.to_string(),
        symbol_idx,
        current_properties,
        current_metadata,
    })
}

fn apply_loaded_metadata_update(
    mut loaded: LoadedSymbolForUpdate,
    next_metadata: SymbolMetadata,
    dry_run: bool,
    operation: &str,
    ctx: &McpContext,
) -> Result<CallToolResult> {
    let next_properties = next_metadata.to_properties_map();
    let changes = diff_metadata(&loaded.current_properties, &next_properties);
    let changed = changes.changed();

    let applied = changed && !dry_run;
    if applied {
        let root_items = kicad_symbol_lib_items_mut(&mut loaded.parsed)
            .ok_or_else(|| anyhow!("Invalid KiCad root structure"))?;
        let symbol_items = root_items
            .get_mut(loaded.symbol_idx)
            .and_then(Sexpr::as_list_mut)
            .ok_or_else(|| anyhow!("Invalid symbol structure for '{}'", loaded.symbol_name))?;
        rewrite_symbol_properties(symbol_items, &next_properties);

        let rendered = format_tree(&loaded.parsed, FormatMode::Normal);
        fs::write(&loaded.path, rendered)
            .with_context(|| format!("Failed to write {}", loaded.path.display()))?;
    }

    ctx.log(
        "info",
        if applied {
            "Updated KiCad symbol metadata"
        } else if dry_run {
            "Computed KiCad symbol metadata changes (dry run)"
        } else {
            "No metadata changes needed"
        },
    );

    Ok(CallToolResult::json(&json!({
        "kicad_sym_path": loaded.kicad_sym_path,
        "symbol_name": loaded.symbol_name,
        "operation": operation,
        "dry_run": dry_run,
        "applied": applied,
        "changed": changed,
        "changes": changes,
        "metadata": next_metadata
    })))
}

fn parse_args<T: DeserializeOwned>(args: Option<Value>, tool_name: &str) -> Result<T> {
    let value = args.unwrap_or_else(|| json!({}));
    serde_json::from_value(value).map_err(|e| anyhow!("{}: invalid arguments: {}", tool_name, e))
}

fn default_true() -> bool {
    true
}

fn select_symbol_name(
    requested: Option<&str>,
    available: &[String],
    path: &Path,
) -> Result<String> {
    match requested {
        Some(name) => {
            if available.iter().any(|candidate| candidate == name) {
                Ok(name.to_string())
            } else {
                bail!(
                    "Symbol '{}' not found in {}. Available symbols: {}",
                    name,
                    path.display(),
                    available.join(", ")
                )
            }
        }
        None => match available {
            [single] => Ok(single.clone()),
            [] => bail!("No symbols found in {}", path.display()),
            _ => bail!(
                "Library {} contains {} symbols. Provide symbol_name. Available symbols: {}",
                path.display(),
                available.len(),
                available.join(", ")
            ),
        },
    }
}

fn parse_kicad_symbol_source(source: &str, path: &Path) -> Result<Sexpr> {
    let parsed =
        pcb_sexpr::parse(source).with_context(|| format!("Failed to parse {}", path.display()))?;
    if kicad_symbol_lib_items(&parsed).is_none() {
        bail!("{} is not a KiCad symbol library", path.display());
    }
    Ok(parsed)
}

fn symbol_index_or_error(root_items: &[Sexpr], symbol_name: &str, path: &Path) -> Result<usize> {
    find_symbol_index(root_items, symbol_name).ok_or_else(|| {
        let available = symbol_names(root_items);
        anyhow!(
            "Symbol '{}' not found in {}. Available symbols: {}",
            symbol_name,
            path.display(),
            available.join(", ")
        )
    })
}

fn symbol_or_error<'a>(
    root_items: &'a [Sexpr],
    symbol_name: &str,
    path: &Path,
) -> Result<&'a [Sexpr]> {
    find_symbol(root_items, symbol_name).ok_or_else(|| {
        let available = symbol_names(root_items);
        anyhow!(
            "Symbol '{}' not found in {}. Available symbols: {}",
            symbol_name,
            path.display(),
            available.join(", ")
        )
    })
}

fn diff_metadata(
    before_map: &BTreeMap<String, String>,
    after_map: &BTreeMap<String, String>,
) -> MetadataChanges {
    let mut changes = MetadataChanges::default();

    let keys: BTreeSet<&String> = before_map.keys().chain(after_map.keys()).collect();
    for key in keys {
        let old = before_map.get(key);
        let new = after_map.get(key);
        if old == new {
            continue;
        }

        if let Some(field) = pcb_eda::kicad::metadata::primary_field_name(key) {
            if new.is_some() {
                changes.primary_set.push(field.to_string());
            } else {
                changes.primary_cleared.push(field.to_string());
            }
        } else if new.is_some() {
            changes.custom_set.push(key.clone());
        } else {
            changes.custom_removed.push(key.clone());
        }
    }

    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_roundtrip_preserves_empty_primary_placeholders() {
        let metadata = SymbolMetadata::from_property_iter(vec![
            ("Reference", "U"),
            ("Value", "ADEX-10"),
            ("Footprint", ""),
            ("Datasheet", ""),
            ("ki_keywords", ""),
            ("ki_fp_filters", ""),
        ]);

        let value = serde_json::to_value(metadata).expect("metadata should serialize");
        let roundtrip: SymbolMetadata =
            serde_json::from_value(value).expect("roundtrip should work");
        let map = roundtrip.to_properties_map();

        assert_eq!(map.get("Footprint"), Some(&"".to_string()));
        assert_eq!(map.get("Datasheet"), Some(&"".to_string()));
        assert_eq!(map.get("ki_keywords"), Some(&"".to_string()));
        assert_eq!(map.get("ki_fp_filters"), Some(&"".to_string()));
    }

    #[test]
    fn merge_patch_preserves_empty_primary_placeholders() {
        let metadata = SymbolMetadata::from_property_iter(vec![
            ("Reference", "U"),
            ("Value", "ADEX-10"),
            ("Footprint", ""),
            ("Datasheet", ""),
            ("Description", "before"),
            ("ki_keywords", ""),
            ("ki_fp_filters", ""),
        ]);

        let mut merged = serde_json::to_value(metadata).expect("metadata should serialize");
        merge(&mut merged, &json!({"primary": {"description": "after"}}));
        let next: SymbolMetadata =
            serde_json::from_value(merged).expect("patch should remain valid");
        let map = next.to_properties_map();

        assert_eq!(map.get("Description"), Some(&"after".to_string()));
        assert_eq!(map.get("Footprint"), Some(&"".to_string()));
        assert_eq!(map.get("Datasheet"), Some(&"".to_string()));
        assert_eq!(map.get("ki_keywords"), Some(&"".to_string()));
        assert_eq!(map.get("ki_fp_filters"), Some(&"".to_string()));
    }

    #[test]
    fn merge_patch_rejects_reserved_custom_property_keys() {
        let metadata = SymbolMetadata::from_property_iter(vec![
            ("Reference", "U"),
            ("Value", "ADEX-10"),
            ("Description", "before"),
        ]);

        let mut merged = serde_json::to_value(metadata).expect("metadata should serialize");
        merge(
            &mut merged,
            &json!({"custom_properties": {"ki_description": "legacy"}}),
        );
        let err = serde_json::from_value::<SymbolMetadata>(merged)
            .expect_err("reserved key should be rejected");
        assert!(err.to_string().contains("ki_description"));
    }

    #[test]
    fn diff_detects_legacy_ki_description_rewrite() {
        let before = BTreeMap::from([
            ("Reference".to_string(), "U".to_string()),
            ("Value".to_string(), "ADEX-10".to_string()),
            ("ki_description".to_string(), "Legacy desc".to_string()),
        ]);
        let normalized = SymbolMetadata::from_property_iter(
            before
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        let after = normalized.to_properties_map();

        let changes = diff_metadata(&before, &after);

        assert!(changes.changed());
        assert_eq!(changes.primary_set, vec!["description".to_string()]);
        assert_eq!(changes.custom_removed, vec!["ki_description".to_string()]);
    }

    #[test]
    fn input_schemas_avoid_top_level_combinators() {
        for tool in tools() {
            let schema = tool
                .input_schema
                .as_object()
                .expect("tool input schema must be a JSON object");
            assert!(
                !schema.contains_key("oneOf"),
                "tool '{}' input schema uses unsupported top-level oneOf",
                tool.name
            );
            assert!(
                !schema.contains_key("allOf"),
                "tool '{}' input schema uses unsupported top-level allOf",
                tool.name
            );
            assert!(
                !schema.contains_key("anyOf"),
                "tool '{}' input schema uses unsupported top-level anyOf",
                tool.name
            );
        }
    }
}
