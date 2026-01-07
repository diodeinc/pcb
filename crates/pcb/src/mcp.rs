use anyhow::Result;
use clap::Args;
use pcb_mcp::{CallToolResult, McpContext, ResourceInfo, ToolInfo};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::build::{build, create_diagnostics_passes};
use crate::file_walker;

#[derive(Args, Debug)]
pub struct McpArgs {}

pub fn execute(_args: McpArgs) -> Result<()> {
    let mut tools = local_tools();

    #[cfg(feature = "api")]
    tools.extend(pcb_diode_api::mcp::tools());

    // Point to public Zener documentation (client fetches directly)
    let resources = vec![ResourceInfo {
        uri: "https://docs.pcb.new/pages/spec".to_string(),
        name: "spec".to_string(),
        title: "Zener Language Specification".to_string(),
        description: "Complete Zener HDL specification: core types, built-in functions, module system, type system, and examples.".to_string(),
        mime_type: "text/html".to_string(),
    }];

    // Run aggregated server that discovers and proxies external MCP servers
    pcb_mcp::run_aggregated_server(tools, resources, |name, args, ctx| {
        if let Some(result) = handle_local(name, args.clone(), ctx) {
            return result;
        }

        #[cfg(feature = "api")]
        {
            pcb_diode_api::mcp::handle(name, args, ctx)
        }

        #[cfg(not(feature = "api"))]
        anyhow::bail!("Unknown tool: {}", name)
    })
}

fn local_tools() -> Vec<ToolInfo> {
    vec![ToolInfo {
        name: "run_layout",
        description: "Sync schematic changes to KiCad and open the layout for interaction. \
            Call this ONLY when you need to: (1) interact with the PCB layout in KiCad, or \
            (2) sync .zen schematic changes to the layout file. Do NOT call this just to build - use 'pcb build' instead.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to a .zen file to process. If omitted, processes all .zen files in the current directory."
                },
                "no_open": {
                    "type": "boolean",
                    "description": "Skip opening KiCad after layout generation (default: false). Set to true if you only need to sync without interacting."
                },
                "sync_board_config": {
                    "type": "boolean",
                    "description": "Apply board config including netclasses (default: true)"
                }
            }
        }),
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "layouts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "source": {"type": "string", "description": "Source .zen file"},
                            "pcb_file": {"type": "string", "description": "Generated .kicad_pcb file path"},
                            "opened": {"type": "boolean", "description": "Whether the layout was opened in KiCad"}
                        }
                    }
                },
                "errors": {
                    "type": "array",
                    "items": {"type": "string"}
                }
            },
            "required": ["layouts"]
        })),
    }]
}

fn handle_local(
    name: &str,
    args: Option<Value>,
    ctx: &McpContext,
) -> Option<Result<CallToolResult>> {
    match name {
        "run_layout" => Some(run_layout(args, ctx)),
        _ => None,
    }
}

fn run_layout(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    use pcb_layout::{process_layout, LayoutError};

    let sync_board_config = args
        .as_ref()
        .and_then(|a| a.get("sync_board_config"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let no_open = args
        .as_ref()
        .and_then(|a| a.get("no_open"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let explicit_path = args
        .as_ref()
        .and_then(|a| a.get("path"))
        .and_then(|v| v.as_str())
        .map(PathBuf::from);

    let paths: Vec<PathBuf> = if let Some(ref path) = explicit_path {
        vec![path.clone()]
    } else {
        file_walker::collect_zen_files(&[] as &[PathBuf], false)?
    };

    if paths.is_empty() {
        anyhow::bail!("No .zen source files found");
    }

    // If no explicit path and multiple files found, return list and ask model to choose
    if explicit_path.is_none() && paths.len() > 1 {
        let available: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        return Ok(CallToolResult::json(&json!({
            "error": "multiple_layouts",
            "message": "Multiple .zen files found. Please call run_layout again with a specific 'path' parameter.",
            "available_files": available
        })));
    }

    // Resolve dependencies using V2 workspace-first architecture
    let (_workspace_info, resolution_result) = crate::resolve::resolve_v2_if_needed(
        paths.first().map(|p| p.as_path()),
        false, // offline
        false, // locked
    )?;

    let mut generated_layouts = Vec::new();
    let mut errors = Vec::new();
    let mut has_errors = false;
    let mut has_warnings = false;

    for zen_path in &paths {
        ctx.log("info", &format!("Processing: {}", zen_path.display()));

        let schematic = match build(
            zen_path,
            false, // offline
            create_diagnostics_passes(&[], &[]),
            false, // deny_warnings
            &mut has_errors,
            &mut has_warnings,
            resolution_result.clone(),
        ) {
            Some(s) => s,
            None => {
                errors.push(format!("{}: build failed", zen_path.display()));
                continue;
            }
        };

        match process_layout(&schematic, zen_path, sync_board_config, false, false) {
            Ok(layout_result) => {
                ctx.log(
                    "info",
                    &format!("Generated: {}", layout_result.pcb_file.display()),
                );

                let opened = if !no_open {
                    if let Err(e) = open::that(&layout_result.pcb_file) {
                        ctx.log("warning", &format!("Failed to open layout: {}", e));
                        false
                    } else {
                        true
                    }
                } else {
                    false
                };

                generated_layouts.push(json!({
                    "source": zen_path.display().to_string(),
                    "pcb_file": layout_result.pcb_file.display().to_string(),
                    "opened": opened
                }));
            }
            Err(LayoutError::NoLayoutPath) => {
                ctx.log(
                    "info",
                    &format!("{}: no layout_path defined, skipping", zen_path.display()),
                );
            }
            Err(e) => {
                errors.push(format!("{}: {}", zen_path.display(), e));
            }
        }
    }

    let mut result = json!({ "layouts": generated_layouts });
    if !errors.is_empty() {
        result["errors"] = json!(errors);
    }

    Ok(CallToolResult::json(&result))
}
