use anyhow::Result;
use pcb_mcp::{CallToolResult, McpContext, ToolInfo};
use pcb_zen::cache_index::{cache_base, ensure_workspace_cache_symlink};
use pcb_zen::ensure_sparse_checkout;
use pcb_zen_core::config::find_workspace_root;
use pcb_zen_core::DefaultFileProvider;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Serialize)]
struct RegistrySearchResult {
    url: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    part_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mpn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dependencies: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dependents: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_path: Option<String>,
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
            description: "Search the Zener package registry for reference designs, modules, and components. Always try this FIRST when adding components to a board. Prefer modules and reference designs over raw components - they include complete implementations. Returns package URLs for use in Module() - dependencies auto-added to pcb.toml. Each result includes cache_path where package source is checked out locally. Before writing .zen code, run `pcb doc spec` to read the language specification. Only use search_component/add_component if nothing found here.",
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
                                "cache_path": {"type": ["string", "null"], "description": "Local path where package source is checked out."}
                            },
                            "required": ["url", "name"]
                        }
                    }
                },
                "required": ["results"]
            })),
        },
        ToolInfo {
            name: "search_component",
            description: "Search Diode's online component database to find components to add to your workspace. IMPORTANT: Only use this AFTER trying search_registry first - registry packages are preferred because they're complete and tested. Use this tool only when: (1) search_registry found no suitable package, or (2) you need a specific part number not in the registry. Returns component_id for use with add_component.",
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
                                "has_ecad_model": {"type": "boolean"},
                                "has_step_model": {"type": "boolean"}
                            },
                            "required": ["component_id", "part_number", "has_ecad_model", "has_step_model"]
                        }
                    }
                },
                "required": ["results"]
            })),
        },
        ToolInfo {
            name: "add_component",
            description:
                "Download a component from Diode's online database and add it to your workspace at ./components/<MFR>/<PART>/<PART>.zen. Requires component_id and part_number from search_component results. Downloads symbol, footprint, 3D model, and datasheet. NOTE: Prefer using packages from search_registry when available - they include complete, tested implementations.",
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
    ]
}

pub fn handle(name: &str, args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    match name {
        "search_registry" => search_registry(args, ctx),
        "search_component" => search_component(args, ctx),
        "add_component" => add_component(args, ctx),
        _ => anyhow::bail!("Unknown tool: {}", name),
    }
}

fn search_registry(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let query = required_str(args.as_ref(), "query")?;

    ctx.log("info", &format!("Searching registry for: {}", query));
    let client = crate::RegistryClient::open()?;
    let results = client.search(&query, 10)?;
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
            match ensure_sparse_checkout(&checkout_dir, &r.url, version, true) {
                Ok(path) => {
                    // If in workspace, remap to workspace-relative path
                    if let Some(ref ws_cache) = workspace_cache {
                        if let Ok(relative) = path.strip_prefix(&cache) {
                            let ws_path = ws_cache.join(relative);
                            if ws_path.exists() {
                                return Some(ws_path);
                            }
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

    let formatted: Vec<_> = results
        .iter()
        .zip(cache_paths.iter())
        .map(|(r, cache_path)| {
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
                url: r.url.clone(),
                name: r.name.clone(),
                category: r.package_category.clone(),
                part_type: r.part_type.clone(),
                mpn: r.mpn.clone(),
                manufacturer: r.manufacturer.clone(),
                description: r
                    .detailed_description
                    .as_ref()
                    .or(r.short_description.as_ref())
                    .cloned(),
                version: r.version.clone(),
                dependencies,
                dependents,
                cache_path: cache_path.as_ref().map(|p| p.display().to_string()),
            }
        })
        .collect();

    Ok(CallToolResult::json(&json!({"results": formatted})))
}

fn search_component(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let part_number = required_str(args.as_ref(), "part_number")?;

    ctx.log("info", &format!("Searching for component: {}", part_number));
    let token = crate::auth::get_valid_token()?;
    let results = crate::search_components(&token, &part_number)?;
    ctx.log("info", &format!("Found {} results", results.len()));

    let formatted: Vec<_> = results
        .iter()
        .map(|r| {
            json!({
                "component_id": r.component_id,
                "part_number": r.part_number,
                "manufacturer": r.manufacturer,
                "description": r.description,
                "package_category": r.package_category,
                "has_ecad_model": r.model_availability.ecad_model,
                "has_step_model": r.model_availability.step_model,
            })
        })
        .collect();

    Ok(CallToolResult::json(&json!({"results": formatted})))
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
