use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub mod aggregator;
pub mod discovery;
pub mod proxy;

pub use aggregator::McpAggregator;
pub use discovery::find_pcb_binaries;
pub use proxy::ExternalMcpServer;

/// Tool definition for tools/list
#[derive(Clone)]
pub struct ToolInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
    pub output_schema: Option<Value>,
}

#[derive(Clone)]
pub struct ResourceInfo {
    pub uri: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub mime_type: String,
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
    #[serde(rename = "image")]
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
    #[serde(rename = "resource_link")]
    ResourceLink {
        uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Value>,
    },
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

    pub fn resource_link(
        uri: &str,
        name: Option<&str>,
        description: Option<&str>,
        mime_type: Option<&str>,
    ) -> Self {
        let text = match (description, name) {
            (Some(desc), Some(_)) => format!("{desc}: {uri}"),
            (Some(desc), None) => format!("{desc}: {uri}"),
            (None, Some(n)) => format!("{n}: {uri}"),
            (None, None) => uri.to_string(),
        };

        Self {
            content: vec![
                CallToolResultContent::Text { text },
                CallToolResultContent::ResourceLink {
                    uri: uri.to_string(),
                    name: name.map(|s| s.to_string()),
                    description: description.map(|s| s.to_string()),
                    mime_type: mime_type.map(|s| s.to_string()),
                    annotations: Some(json!({
                        "audience": ["assistant"],
                        "priority": 0.9
                    })),
                },
            ],
            structured_content: Some(json!({
                "uri": uri
            })),
            is_error: false,
        }
    }
}

/// Run MCP server on stdin/stdout
pub fn run_server<F>(tools: &[ToolInfo], resources: &[ResourceInfo], handler: F) -> Result<()>
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
                    "capabilities": {"tools": {}, "logging": {}, "resources": {}}
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
            "resources/list" => {
                let resource_list: Vec<_> = resources
                    .iter()
                    .map(|r| {
                        json!({
                            "uri": r.uri,
                            "name": r.name,
                            "title": r.title,
                            "description": r.description,
                            "mimeType": r.mime_type,
                        })
                    })
                    .collect();

                json!({"jsonrpc": "2.0", "id": id, "result": {"resources": resource_list}})
            }
            "resources/read" => {
                // All our resources are HTTPS URLs that clients should fetch directly
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "HTTPS resources should be fetched by client"}
                })
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

        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }

    Ok(())
}

/// Run an MCP server that aggregates built-in tools with discovered external MCP servers
///
/// External servers are discovered by scanning PATH for `pcb-*` binaries and
/// attempting to spawn them with an `mcp` subcommand. Tools from external servers
/// are namespaced as `servername_toolname`.
pub fn run_aggregated_server<F>(
    builtin_tools: Vec<ToolInfo>,
    builtin_resources: Vec<ResourceInfo>,
    builtin_handler: F,
) -> Result<()>
where
    F: Fn(&str, Option<Value>, &McpContext) -> Result<CallToolResult>,
{
    let mut aggregator = McpAggregator::new(builtin_tools, builtin_resources, builtin_handler);

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
                    "capabilities": {"tools": {}, "logging": {}, "resources": {}}
                }
            }),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "logging/setLevel" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => {
                let tools = aggregator.all_tools();
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
            "resources/list" => {
                let resources = aggregator.all_resources();
                let resource_list: Vec<_> = resources
                    .iter()
                    .map(|r| {
                        json!({
                            "uri": r.uri,
                            "name": r.name,
                            "title": r.title,
                            "description": r.description,
                            "mimeType": r.mime_type,
                        })
                    })
                    .collect();

                json!({"jsonrpc": "2.0", "id": id, "result": {"resources": resource_list}})
            }
            "resources/read" => {
                // All our resources are HTTPS URLs that clients should fetch directly
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32601, "message": "HTTPS resources should be fetched by client"}
                })
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
                    Some(name) => match aggregator.handle_tool_call(name, args, &ctx) {
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

        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }

    Ok(())
}
