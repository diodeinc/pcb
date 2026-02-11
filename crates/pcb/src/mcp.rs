use anyhow::Result;
use clap::{Args, Subcommand};
use pcb_mcp::{CallToolResult, McpContext, ResourceInfo, ToolHandler, ToolInfo};
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::build::{build, create_diagnostics_passes};
use crate::file_walker;

#[derive(Args, Debug)]
pub struct McpArgs {
    #[command(subcommand)]
    command: Option<McpCommand>,
}

#[derive(Subcommand, Debug)]
enum McpCommand {
    /// Evaluate JavaScript code with access to MCP tools
    Eval(EvalArgs),
}

#[derive(Args, Debug)]
struct EvalArgs {
    /// JavaScript code to execute (use '-' to read from stdin)
    code: Option<String>,

    /// Read code from a file
    #[arg(short, long)]
    file: Option<PathBuf>,
}

pub fn execute(args: McpArgs) -> Result<()> {
    match args.command {
        Some(McpCommand::Eval(eval_args)) => execute_eval(eval_args),
        None => execute_server(),
    }
}

fn execute_eval(args: EvalArgs) -> Result<()> {
    use std::io::Read;

    let code = match (&args.code, &args.file) {
        (Some(code), None) if code == "-" => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
        (Some(code), None) => code.clone(),
        (None, Some(file)) => std::fs::read_to_string(file)?,
        (Some(_), Some(_)) => anyhow::bail!("Cannot specify both code argument and --file"),
        (None, None) => anyhow::bail!("Must provide code argument or --file"),
    };

    let (tools, resources, handler) = create_tool_config();
    let result = pcb_mcp::eval_js(&code, tools, resources, handler)?;

    for log in &result.logs {
        eprintln!("{}", log);
    }

    if result.is_error {
        if let Some(msg) = &result.error_message {
            eprintln!("Error: {}", msg);
        }
        std::process::exit(1);
    }

    println!("{}", serde_json::to_string_pretty(&result.value)?);
    Ok(())
}

fn create_tool_config() -> (Vec<ToolInfo>, Vec<ResourceInfo>, ToolHandler) {
    let mut tools = local_tools();

    #[cfg(feature = "api")]
    tools.extend(pcb_diode_api::mcp::tools());

    let resources = vec![ResourceInfo {
        uri: "https://docs.pcb.new/llms.txt".to_string(),
        name: "zener-docs".to_string(),
        title: "Zener Language Documentation".to_string(),
        description: "Complete Zener HDL documentation".to_string(),
        mime_type: "text/plain".to_string(),
    }];

    let handler: ToolHandler = Box::new(|name, args, ctx| {
        if let Some(result) = handle_local(name, args.clone(), ctx) {
            return result;
        }

        #[cfg(feature = "api")]
        {
            pcb_diode_api::mcp::handle(name, args, ctx)
        }

        #[cfg(not(feature = "api"))]
        anyhow::bail!("Unknown tool: {}", name)
    });

    (tools, resources, handler)
}

fn execute_server() -> Result<()> {
    let (tools, resources, handler) = create_tool_config();
    pcb_mcp::run_aggregated_server(tools, resources, handler)
}

fn local_tools() -> Vec<ToolInfo> {
    vec![
        ToolInfo {
            name: "get_skill",
            description: "Get instructions and documentation for working with PCB designs in the Zener hardware description language. \
                Returns context on CLI commands, language concepts, and available MCP tools. \
                Call this at the start of a conversation involving .zen files or PCB design.",
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
            output_schema: None,
        },
        ToolInfo {
        name: "run_layout",
        description: "Sync schematic changes to KiCad and open the layout for interaction. \
            Call this ONLY when you need to: (1) interact with the PCB layout in KiCad, or \
            (2) sync .zen schematic changes to the layout file. Do NOT call this just to build - use 'pcb build' instead.",
        input_schema: json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Path to a .zen file to process"
                },
                "no_open": {
                    "type": "boolean",
                    "description": "Skip opening KiCad after layout generation (default: false). Set to true if you only need to sync without interacting."
                },
                "sync_board_config": {
                    "type": "boolean",
                    "description": "Apply board config including netclasses (default: true)"
                }
            },
            "required": ["file"]
        }),
        output_schema: Some(json!({
            "type": "object",
            "properties": {
                "pcb_file": {"type": "string", "description": "Generated .kicad_pcb file path"},
                "opened": {"type": "boolean", "description": "Whether the layout was opened in KiCad"},
                "error": {"type": "string", "description": "Error message if layout failed"}
            }
        })),
    },
    ]
}

fn handle_local(
    name: &str,
    args: Option<Value>,
    ctx: &McpContext,
) -> Option<Result<CallToolResult>> {
    match name {
        "get_skill" => Some(Ok(CallToolResult {
            content: vec![pcb_mcp::CallToolResultContent::Text {
                text: crate::run::AGENTS_SKILL_MD.to_string(),
            }],
            structured_content: None,
            is_error: false,
        })),
        "run_layout" => Some(run_layout(args, ctx)),
        _ => None,
    }
}

fn run_layout(args: Option<Value>, ctx: &McpContext) -> Result<CallToolResult> {
    let args = args.as_ref();
    let get_str = |key| args.and_then(|a| a.get(key)).and_then(|v| v.as_str());
    let get_bool = |key, default| {
        args.and_then(|a| a.get(key))
            .and_then(|v| v.as_bool())
            .unwrap_or(default)
    };

    let zen_path = PathBuf::from(
        get_str("file").ok_or_else(|| anyhow::anyhow!("Missing required 'file' parameter"))?,
    );
    file_walker::require_zen_file(&zen_path)?;

    let sync_board_config = get_bool("sync_board_config", true);
    let no_open = get_bool("no_open", false);

    let resolution_result = crate::resolve::resolve(zen_path.parent(), false, false)?;

    let mut has_errors = false;
    let mut has_warnings = false;
    let Some(schematic) = build(
        &zen_path,
        create_diagnostics_passes(&[], &[]),
        false,
        &mut has_errors,
        &mut has_warnings,
        resolution_result,
    ) else {
        return Ok(CallToolResult::json(&json!({ "error": "Build failed" })));
    };

    let mut diagnostics = pcb_zen_core::Diagnostics::default();
    match pcb_layout::process_layout(
        &schematic,
        sync_board_config,
        false,
        false,
        &mut diagnostics,
    ) {
        Ok(Some(result)) => {
            ctx.log("info", &format!("Generated: {}", result.pcb_file.display()));
            let opened = !no_open && open::that(&result.pcb_file).is_ok();
            Ok(CallToolResult::json(&json!({
                "pcb_file": result.pcb_file.display().to_string(),
                "opened": opened
            })))
        }
        Ok(None) => Ok(CallToolResult::json(
            &json!({ "error": "No layout_path defined in design" }),
        )),
        Err(e) => Ok(CallToolResult::json(&json!({ "error": e.to_string() }))),
    }
}
