use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use pcb_mcp::{CallToolResult, McpContext, ToolHandler, ToolInfo};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::build::{build, create_diagnostics_passes};
use crate::file_walker;

const CODEMODE_ONLY_TOOLS: &[&str] = &[
    "read_kicad_symbol_metadata",
    "write_kicad_symbol_metadata",
    "merge_kicad_symbol_metadata",
];

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
        write_images_for_eval_result(&mut value, &result.images, args.output_dir.as_deref())?;

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

fn write_images_for_eval_result(
    value: &mut Value,
    images: &[pcb_mcp::ImageData],
    output_dir: Option<&Path>,
) -> Result<(PathBuf, usize)> {
    let output_dir = resolve_eval_output_dir(output_dir)?;
    let images_written = {
        let mut rewriter = ImageFileRewriter::new(images, &output_dir);
        rewriter.rewrite_value(value)?;
        rewriter.images_written
    };
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

fn write_images_from_value_markers(
    rewriter: &mut ImageFileRewriter<'_>,
    value: &mut Value,
) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                write_images_from_value_markers(rewriter, item)?;
            }
        }
        Value::Object(obj) => {
            if let Some(replacement) = image_marker_to_file_value(rewriter, obj)? {
                *value = replacement;
                rewriter.images_written += 1;
                return Ok(());
            }

            for child in obj.values_mut() {
                write_images_from_value_markers(rewriter, child)?;
            }
        }
        _ => {}
    }

    Ok(())
}

fn image_marker_to_file_value(
    rewriter: &mut ImageFileRewriter<'_>,
    obj: &serde_json::Map<String, Value>,
) -> Result<Option<Value>> {
    let kind = obj.get("type").and_then(|v| v.as_str());
    if kind != Some("image") {
        return Ok(None);
    }

    if let Some(idx) = obj.get("imageIndex").and_then(parse_image_index) {
        let image = rewriter
            .images
            .get(idx)
            .ok_or_else(|| anyhow::anyhow!("Image marker references missing image index {idx}"))?;
        let replacement = rewriter.write_image_file_from_base64(&image.data, &image.mime_type)?;
        return Ok(Some(replacement));
    }

    if let (Some(data), Some(mime_type)) = (
        obj.get("data").and_then(|v| v.as_str()),
        obj.get("mimeType").and_then(|v| v.as_str()),
    ) {
        let replacement = rewriter.write_image_file_from_base64(data, mime_type)?;
        return Ok(Some(replacement));
    }

    Ok(None)
}

fn parse_image_index(value: &Value) -> Option<usize> {
    if let Some(idx) = value.as_u64() {
        return Some(idx as usize);
    }
    if let Some(idx) = value.as_i64()
        && idx >= 0
    {
        return Some(idx as usize);
    }
    if let Some(idx) = value.as_f64()
        && idx.is_finite()
        && idx >= 0.0
        && idx.fract() == 0.0
    {
        return Some(idx as usize);
    }
    None
}

struct ImageFileRewriter<'a> {
    images: &'a [pcb_mcp::ImageData],
    output_dir: &'a Path,
    next_file_index: usize,
    images_written: usize,
}

impl<'a> ImageFileRewriter<'a> {
    fn new(images: &'a [pcb_mcp::ImageData], output_dir: &'a Path) -> Self {
        Self {
            images,
            output_dir,
            next_file_index: 0,
            images_written: 0,
        }
    }

    fn rewrite_value(&mut self, value: &mut Value) -> Result<()> {
        write_images_from_value_markers(self, value)
    }

    fn write_image_file_from_base64(&mut self, data: &str, mime_type: &str) -> Result<Value> {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("Failed to decode image base64")?;

        self.next_file_index += 1;
        let ext = file_extension_for_mime_type(mime_type);
        let path = self
            .output_dir
            .join(format!("inline_image_{:03}.{}", self.next_file_index, ext));
        std::fs::create_dir_all(self.output_dir).with_context(|| {
            format!(
                "Failed to create output directory {}",
                self.output_dir.display()
            )
        })?;
        std::fs::write(&path, bytes)
            .with_context(|| format!("Failed to write image to {}", path.display()))?;

        Ok(json!({
            "type": "image_file",
            "mimeType": mime_type,
            "path": path.display().to_string(),
        }))
    }
}

fn create_tool_config() -> (Vec<ToolInfo>, ToolHandler) {
    #[allow(unused_mut)]
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
    let direct_tools: Vec<ToolInfo> = tools
        .iter()
        .filter(|tool| !CODEMODE_ONLY_TOOLS.contains(&tool.name))
        .cloned()
        .collect();
    pcb_mcp::run_aggregated_server(tools, direct_tools, vec![], handler)
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

    let resolution_result = crate::resolve::resolve(Some(&zen_path), false, false)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_image_index_marker_to_image_file() {
        let temp = tempfile::tempdir().unwrap();
        let mut value = json!({
            "preview": {
                "type": "image",
                "mimeType": "image/png",
                "imageIndex": 0
            }
        });
        let images = vec![pcb_mcp::ImageData {
            data: "AA==".to_string(),
            mime_type: "image/png".to_string(),
        }];

        let (output_dir, images_written) =
            write_images_for_eval_result(&mut value, &images, Some(temp.path())).unwrap();

        assert_eq!(images_written, 1);
        assert_eq!(output_dir, temp.path().to_path_buf());
        assert_eq!(value["preview"]["type"], "image_file");
        assert_eq!(value["preview"]["mimeType"], "image/png");
        assert!(value["preview"].get("imageIndex").is_none());

        let written_path = PathBuf::from(value["preview"]["path"].as_str().unwrap());
        assert!(written_path.exists(), "expected image file to be written");
        assert_eq!(std::fs::read(&written_path).unwrap(), vec![0u8]);
    }
}
