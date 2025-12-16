use anyhow::Result;
use pcb_mcp::{CallToolResult, McpContext, ToolInfo};
use serde_json::{json, Value};

fn required_str(args: Option<&Value>, key: &str) -> Result<String> {
    args.and_then(|a| a.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| anyhow::anyhow!("{} required", key))
}

fn get_zener_docs(_ctx: &McpContext) -> Result<CallToolResult> {
    // Return simple text content for compatibility with AMP Code
    // (AMP Code doesn't support resource_link content type yet)
    Ok(CallToolResult::json(&json!({
        "uri": "https://docs.pcb.new/pages/spec",
        "name": "Zener Language Specification",
        "description": "Complete Zener language specification including syntax, built-in functions, core types (Net, Component, Symbol, Interface, Module), module system, type system, and examples."
    })))
}

pub fn tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "get_zener_docs",
            description: "Get the Zener language specification and documentation. Returns a link to the complete language reference including syntax, built-in functions, core types (Net, Component, Symbol, Interface, Module), module system, and examples.",
            input_schema: json!({
                "type": "object",
                "properties": {},
            }),
            output_schema: Some(json!({
                "type": "object",
                "properties": {
                    "uri": {"type": "string", "description": "URI to the Zener documentation resource"}
                }
            })),
        },
        ToolInfo {
            name: "search_component",
            description: "Search Diode's component database for electronic parts by manufacturer part number (MPN), component name, or keyword. Returns component IDs, datasheets, and model availability. Use this first to find component_id before calling add_component.",
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
                "Download a component from Diode's database (requires component_id and part_number from search_component) and add it to the workspace as a .zen file at ./components/<PART>/<PART>.zen. This downloads the full component definition including symbol, footprint, datasheet links, and electrical properties.",
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
        "search_component" => search_component(args, ctx),
        "add_component" => add_component(args, ctx),
        _ => anyhow::bail!("Unknown tool: {}", name),
    }
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
    )?;

    ctx.log(
        "info",
        &format!("Component added to {}", result.component_path.display()),
    );

    Ok(CallToolResult::json(&json!({
        "path": result.component_path.display().to_string()
    })))
}
