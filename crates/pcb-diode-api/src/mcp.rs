use anyhow::Result;
use pcb_mcp::{CallToolResult, McpContext, ToolInfo};
use serde_json::{json, Value};

fn required_str(args: Option<&Value>, key: &str) -> Result<String> {
    args.and_then(|a| a.get(key))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .ok_or_else(|| anyhow::anyhow!("{} required", key))
}

pub fn tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "search_component",
            description: "Search for electronic components by part number",
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
                                "description": {"type": ["string", "null"]},
                                "package_category": {"type": ["string", "null"]},
                                "datasheets": {"type": "array", "items": {"type": "string"}},
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
                "Download and add a component to the workspace at ./components/<PART>/<PART>.zen",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "component_id": {
                        "type": "string",
                        "description": "Component ID from search_component results"
                    }
                },
                "required": ["component_id"]
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
                "description": r.description,
                "package_category": r.package_category,
                "datasheets": r.datasheets,
                "has_ecad_model": r.model_availability.ecad_model,
                "has_step_model": r.model_availability.step_model,
            })
        })
        .collect();

    Ok(CallToolResult::json(&json!({"results": formatted})))
}

fn add_component(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let component_id = required_str(args.as_ref(), "component_id")?;

    ctx.log("info", "Authenticating...");
    let token = crate::auth::get_valid_token()?;

    ctx.progress(1, 4, "Searching for component");
    let results = crate::search_components(&token, &component_id)?;
    let component = results
        .into_iter()
        .find(|c| c.component_id == component_id)
        .ok_or_else(|| anyhow::anyhow!("Component not found"))?;

    ctx.log(
        "info",
        &format!("Adding component: {}", component.part_number),
    );
    ctx.progress(2, 4, "Downloading component data");

    let workspace = std::env::current_dir()?;

    ctx.progress(3, 4, "Processing files");
    let result = crate::add_component_to_workspace(&token, &component, &workspace)?;

    ctx.progress(4, 4, "Complete");
    ctx.log(
        "info",
        &format!("Component added to {}", result.component_path.display()),
    );

    Ok(CallToolResult::json(&json!({
        "path": result.component_path.display().to_string()
    })))
}
