use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use pcb_mcp::{CallToolResult, McpContext, ToolHandler, ToolInfo};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

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

    /// Directory to write image artifacts from render-like tool results
    #[arg(long)]
    output_dir: Option<PathBuf>,
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

    let (tools, handler) = create_tool_config();
    let result = pcb_mcp::eval_js(&code, tools, vec![], handler)?;

    for log in &result.logs {
        eprintln!("{}", log);
    }

    if result.is_error {
        if let Some(msg) = &result.error_message {
            eprintln!("Error: {}", msg);
        }
        std::process::exit(1);
    }

    if should_render_inline_images(&args, &result) {
        render_inline_images_to_terminal(&result.images)?;
        return Ok(());
    }

    let mut value = result.value;
    let (output_dir, images_written) =
        write_images_from_result_value(&mut value, args.output_dir.as_deref())?;

    if images_written == 0 && args.output_dir.is_none() {
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let output = json!({
        "ok": true,
        "value": value,
        "images_written": images_written,
        "output_dir": output_dir.display().to_string(),
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn should_render_inline_images(args: &EvalArgs, result: &pcb_mcp::ExecutionResult) -> bool {
    args.output_dir.is_none()
        && !result.images.is_empty()
        && result
            .images
            .iter()
            .all(|image| image.mime_type == "image/png")
        && crate::tty::is_interactive()
        && matches!(detect_inline_image_protocol(), InlineImageProtocol::Kitty)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineImageProtocol {
    Kitty,
    None,
}

fn detect_inline_image_protocol() -> InlineImageProtocol {
    if let Ok(term) = std::env::var("TERM") {
        let term = term.to_lowercase();
        if term.contains("kitty") || term.contains("ghostty") {
            return InlineImageProtocol::Kitty;
        }
    }
    if let Ok(program) = std::env::var("TERM_PROGRAM")
        && program.to_lowercase().contains("ghostty")
    {
        return InlineImageProtocol::Kitty;
    }
    InlineImageProtocol::None
}

fn render_inline_images_to_terminal(images: &[pcb_mcp::ImageData]) -> Result<()> {
    for image in images {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&image.data)
            .context("Failed to decode image for inline terminal rendering")?;
        render_kitty_png(&bytes)?;
    }
    Ok(())
}

fn render_kitty_png(png_bytes: &[u8]) -> Result<()> {
    use base64::Engine;

    let encoded = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    let mut stdout = std::io::stdout().lock();
    let mut i = 0usize;
    let mut first_chunk = true;
    while i < encoded.len() {
        let end = std::cmp::min(i + 4096, encoded.len());
        let more = if end < encoded.len() { 1 } else { 0 };
        if first_chunk {
            write!(
                stdout,
                "\x1b_Gf=100,a=T,m={};{}\x1b\\",
                more,
                &encoded[i..end]
            )?;
            first_chunk = false;
        } else {
            write!(stdout, "\x1b_Gm={};{}\x1b\\", more, &encoded[i..end])?;
        }
        i = end;
    }
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

fn write_images_from_result_value(
    value: &mut Value,
    output_dir: Option<&Path>,
) -> Result<(PathBuf, usize)> {
    let output_dir = resolve_eval_output_dir(output_dir)?;

    let mut image_index = 0usize;
    let images_written = write_images_recursively(value, &output_dir, &mut image_index)?;
    Ok((output_dir, images_written))
}

fn resolve_eval_output_dir(output_dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = output_dir {
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        return Ok(std::env::current_dir()?.join(path));
    }

    Ok(std::env::temp_dir()
        .join("pcb-mcp-eval-artifacts")
        .join("inline"))
}

fn file_extension_for_mime_type(mime_type: &str) -> &'static str {
    match mime_type {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

fn write_images_recursively(
    value: &mut Value,
    output_dir: &Path,
    next_index: &mut usize,
) -> Result<usize> {
    let mut written = 0usize;
    match value {
        Value::Array(items) => {
            for item in items {
                written += write_images_recursively(item, output_dir, next_index)?;
            }
        }
        Value::Object(obj) => {
            let is_image = obj
                .get("type")
                .and_then(|v| v.as_str())
                .map(|t| t == "image")
                .unwrap_or(false);
            if is_image {
                let data = obj
                    .get("data")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing image data"))?;
                let mime_type = obj
                    .get("mimeType")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("Missing image mimeType"))?;

                use base64::Engine;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .context("Failed to decode image base64")?;

                *next_index += 1;
                let ext = file_extension_for_mime_type(mime_type);
                let path = output_dir.join(format!("inline_image_{:03}.{}", *next_index, ext));
                std::fs::create_dir_all(output_dir).with_context(|| {
                    format!("Failed to create output directory {}", output_dir.display())
                })?;
                std::fs::write(&path, bytes)
                    .with_context(|| format!("Failed to write image to {}", path.display()))?;

                *value = json!({
                    "type": "image_file",
                    "mimeType": mime_type,
                    "path": path.display().to_string(),
                });
                written += 1;
            } else {
                for child in obj.values_mut() {
                    written += write_images_recursively(child, output_dir, next_index)?;
                }
            }
        }
        _ => {}
    }
    Ok(written)
}

fn create_tool_config() -> (Vec<ToolInfo>, ToolHandler) {
    let mut tools = local_tools();

    #[cfg(feature = "api")]
    tools.extend(pcb_diode_api::mcp::tools());

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

    (tools, handler)
}

fn execute_server() -> Result<()> {
    let (tools, handler) = create_tool_config();
    pcb_mcp::run_aggregated_server(tools, vec![], handler)
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
    match pcb_layout::process_layout(&schematic, false, false, &mut diagnostics) {
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
