use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

/// Tool definition for tools/list
pub struct ToolInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
}

/// Context passed to tool handlers
pub struct McpContext {
    progress_token: Option<String>,
}

impl McpContext {
    pub fn log(&self, level: &str, message: &str) {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": {
                "level": level,
                "logger": "pcb",
                "data": {"message": message}
            }
        });
        eprintln!("{}", notification);
    }

    pub fn progress(&self, progress: u64, total: u64, message: &str) {
        if let Some(token) = &self.progress_token {
            let notification = json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {
                    "progressToken": token,
                    "progress": progress,
                    "total": total,
                    "message": message
                }
            });
            eprintln!("{}", notification);
        }
    }
}

/// Tool execution result
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    pub content: Vec<CallToolResultContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CallToolResultContent {
    #[serde(rename = "text")]
    Text { text: String },
}

impl CallToolResult {
    pub fn json(value: &Value) -> Self {
        Self {
            content: vec![CallToolResultContent::Text {
                text: value.to_string(),
            }],
            structured_content: Some(value.clone()),
            is_error: false,
        }
    }

    pub fn error(message: &str) -> Self {
        Self {
            content: vec![CallToolResultContent::Text {
                text: message.to_string(),
            }],
            structured_content: None,
            is_error: true,
        }
    }
}

/// Run MCP server on stdin/stdout
pub fn run_server<F>(tools: &[ToolInfo], handler: F) -> Result<()>
where
    F: Fn(&str, Option<Value>, &McpContext) -> Result<CallToolResult>,
{
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Notifications (no id) are ignored
        if req.get("id").is_none() {
            continue;
        }

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");

        let response = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {"name": "pcb-mcp", "version": env!("CARGO_PKG_VERSION")},
                    "capabilities": {"tools": {}, "logging": {}}
                }
            }),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "logging/setLevel" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => {
                let tool_list: Vec<_> = tools
                    .iter()
                    .map(|t| {
                        let mut tool = json!({
                            "name": t.name,
                            "description": t.description,
                            "inputSchema": t.input_schema
                        });
                        if let Some(schema) = &t.output_schema {
                            tool.as_object_mut()
                                .unwrap()
                                .insert("outputSchema".to_string(), schema.clone());
                        }
                        tool
                    })
                    .collect();

                json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tool_list}})
            }
            "tools/call" => {
                let params = req.get("params");
                let name = params.and_then(|p| p.get("name")).and_then(|v| v.as_str());
                let args = params.and_then(|p| p.get("arguments").cloned());
                let progress_token = params
                    .and_then(|p| p.get("_meta"))
                    .and_then(|m| m.get("progressToken"))
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string());

                let ctx = McpContext { progress_token };

                match name {
                    Some(name) => match handler(name, args, &ctx) {
                        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                        Err(e) => {
                            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32000, "message": e.to_string()}})
                        }
                    },
                    None => {
                        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "Missing tool name"}})
                    }
                }
            }
            _ => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}})
            }
        };

        writeln!(stdout, "{}", response)?;
        stdout.flush()?;
    }

    Ok(())
}
