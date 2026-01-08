use anyhow::Result;
use pcb_mcp::{CallToolResult, McpContext, ToolInfo};
use pcb_zen::cache_index::cache_base;
use pcb_zen::ensure_sparse_checkout;
use rayon::prelude::*;
use serde_json::{json, Value};
use std::path::PathBuf;

fn required_str(args: Option<&Value>, key: &str) -> Result<String> {
    args.and_then(|a| a.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| anyhow::anyhow!("{} required", key))
}

fn get_zener_docs(_ctx: &McpContext) -> Result<CallToolResult> {
    Ok(CallToolResult::json(&json!({
        "docs": [
            {
                "uri": "https://docs.pcb.new/pages/spec",
                "name": "Zener Language Specification",
                "description": "Complete language reference: syntax, built-in functions, core types (Net, Component, Symbol, Interface, Module), and code examples."
            },
            {
                "uri": "https://docs.pcb.new/pages/packages",
                "name": "Packages",
                "description": "Package management, workspaces, and dependency resolution: pcb.toml manifests, version constraints, MVS algorithm, lockfiles (pcb.sum), and CLI commands."
            },
            {
                "uri": "https://docs.pcb.new/pages/testing",
                "name": "Testing",
                "description": "TestBench for module validation, circuit graph analysis, and path validation for verifying connectivity and topology."
            }
        ]
    })))
}

pub fn tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "get_zener_docs",
            description: "Get Zener language documentation links. Returns URIs to the language specification (syntax, types, built-ins) and versioning guide (dependencies, pcb.toml, lockfiles).",
            input_schema: json!({
                "type": "object",
                "properties": {},
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "docs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "uri": {"type": "string", "description": "URI to fetch the documentation"},
                                "name": {"type": "string", "description": "Documentation page name"},
                                "description": {"type": "string", "description": "What this documentation covers"}
                            },
                            "required": ["uri", "name", "description"]
                        }
                    }
                },
                "required": ["docs"]
            })),
        },
        ToolInfo {
            name: "search_registry",
            description: "IMPORTANT: Always try this tool FIRST when the user asks to add any component, module, or circuit block to their board. Search the Zener package registry for existing reference designs, modules, and components. Prefer modules and reference designs over raw components - they include complete implementations with all supporting parts. Only fall back to components when no suitable module/reference exists. Registry packages are vetted and tested. Returns package URLs that can be used directly in load() and Module() - the dependency will automatically be added to pcb.toml by the toolchain. Each result includes a cache_path where the package source is checked out locally - read files from this path to understand how to use the package. Only use search_component/add_component if this registry search doesn't find a suitable package.",
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
                                "url": {"type": "string", "description": "Package URL for use in load() or Module()"},
                                "name": {"type": "string"},
                                "category": {"type": ["string", "null"], "description": "Package type: component, module, or reference"},
                                "part_type": {"type": ["string", "null"], "description": "Component type (e.g., 'voltage regulator', 'MOSFET', 'connector')"},
                                "mpn": {"type": ["string", "null"]},
                                "manufacturer": {"type": ["string", "null"]},
                                "description": {"type": ["string", "null"]},
                                "version": {"type": ["string", "null"]},
                                "dependencies": {"type": "array", "items": {"type": "string"}, "description": "Package URLs this depends on"},
                                "dependents": {"type": "array", "items": {"type": "string"}, "description": "Package URLs that use this"},
                                "cache_path": {"type": ["string", "null"], "description": "Local cache path where the package source is checked out. Read files from this path to understand how to use the package."}
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
        "get_zener_docs" => get_zener_docs(ctx),
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

    // Ensure packages are checked out in parallel
    let cache = cache_base();
    let cache_paths: Vec<Option<PathBuf>> = results
        .par_iter()
        .map(|r| {
            let version = r.version.as_deref()?;
            let checkout_dir = cache.join(&r.url).join(version);
            match ensure_sparse_checkout(&checkout_dir, &r.url, version, true) {
                Ok(path) => Some(path),
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
            // Fetch dependencies and dependents for each result
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

            json!({
                "url": r.url,
                "name": r.name,
                "category": r.package_category,
                "part_type": r.part_type,
                "mpn": r.mpn,
                "manufacturer": r.manufacturer,
                "description": r.detailed_description.as_ref().or(r.short_description.as_ref()),
                "version": r.version,
                "dependencies": dependencies,
                "dependents": dependents,
                "cache_path": cache_path.as_ref().map(|p| p.display().to_string()),
            })
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
