use crate::{CallToolResult, ResourceInfo, ToolInfo};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

/// Default timeout for MCP requests during initialization/discovery
const INIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Default timeout for tool calls (may take longer)
const TOOL_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// An external MCP server running as a child process
pub struct ExternalMcpServer {
    /// Name derived from binary (e.g., "mycmd" from "pcb-mycmd")
    pub name: String,
    /// The binary name (e.g., "pcb-mycmd")
    pub binary: String,
    /// Child process handle
    child: Child,
    /// Buffered writer to child's stdin
    stdin: BufWriter<ChildStdin>,
    /// Channel receiver for lines read from stdout
    response_rx: mpsc::Receiver<std::io::Result<String>>,
    /// Request ID counter
    request_id: AtomicU64,
    /// Tools discovered from this server
    pub tools: Vec<ExternalToolInfo>,
    /// Resources discovered from this server
    pub resources: Vec<ExternalResourceInfo>,
}

/// Tool info from external server (owned strings, not static)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalToolInfo {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

/// Resource info from external server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalResourceInfo {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "mimeType", default)]
    pub mime_type: String,
}

impl ExternalMcpServer {
    /// Spawn an external MCP server and initialize it
    ///
    /// The binary should support a `mcp` subcommand that runs an MCP server
    /// on stdin/stdout.
    pub fn spawn(binary: &str) -> Result<Self> {
        let mut child = Command::new(binary)
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()) // Ignore stderr for now
            .spawn()
            .with_context(|| format!("Failed to spawn {}", binary))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdin for {}", binary))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Failed to get stdout for {}", binary))?;

        // Extract name from binary path: "/path/to/pcb-sym" -> "sym"
        let filename = Path::new(binary)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(binary);
        let name = filename
            .strip_prefix("pcb-")
            .unwrap_or(filename)
            .to_string();

        // Spawn a reader thread that sends lines through a channel
        // This allows us to use recv_timeout for non-blocking reads
        let (tx, rx) = mpsc::channel();
        let mut reader = BufReader::new(stdout);
        thread::spawn(move || {
            loop {
                let mut line = String::new();
                let result = reader.read_line(&mut line);
                let should_stop = result.is_err() || line.is_empty();
                // Send the result (trim the line since read_line includes newline)
                if tx.send(result.map(|_| line)).is_err() {
                    break; // Receiver dropped
                }
                if should_stop {
                    break;
                }
            }
        });

        let mut server = Self {
            name,
            binary: binary.to_string(),
            child,
            stdin: BufWriter::new(stdin),
            response_rx: rx,
            request_id: AtomicU64::new(1),
            tools: Vec::new(),
            resources: Vec::new(),
        };

        // Initialize the connection
        server.initialize()?;

        // Discover tools and resources
        server.discover()?;

        Ok(server)
    }

    /// Send initialize request
    fn initialize(&mut self) -> Result<()> {
        let _response = self.send_request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "clientInfo": {
                    "name": "pcb-mcp-proxy",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {}
            }),
        )?;

        // Send initialized notification
        self.send_notification("notifications/initialized", json!({}))?;

        Ok(())
    }

    /// Discover tools and resources from the server
    fn discover(&mut self) -> Result<()> {
        // Get tools
        let tools_response = self.send_request("tools/list", json!({}))?;
        if let Some(tools) = tools_response.get("tools") {
            self.tools = serde_json::from_value(tools.clone()).unwrap_or_default();
        }

        // Get resources
        let resources_response = self.send_request("resources/list", json!({}))?;
        if let Some(resources) = resources_response.get("resources") {
            self.resources = serde_json::from_value(resources.clone()).unwrap_or_default();
        }

        Ok(())
    }

    /// Send a JSON-RPC request and wait for response with default timeout
    fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.send_request_with_timeout(method, params, INIT_TIMEOUT)
    }

    /// Send a JSON-RPC request and wait for response with custom timeout
    fn send_request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::SeqCst);

        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        // Write request
        writeln!(self.stdin, "{}", request)?;
        self.stdin.flush()?;

        // Read response with timeout
        let line = self
            .response_rx
            .recv_timeout(timeout)
            .map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => {
                    anyhow!(
                        "Timeout waiting for response from {} ({}s)",
                        self.binary,
                        timeout.as_secs()
                    )
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    anyhow!("Server {} disconnected unexpectedly", self.binary)
                }
            })?
            .with_context(|| format!("Failed to read from {}", self.binary))?;

        let response: Value = serde_json::from_str(&line)
            .with_context(|| format!("Invalid JSON response from {}: {}", self.binary, line))?;

        // Check for error
        if let Some(error) = response.get("error") {
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            return Err(anyhow!("MCP error from {}: {}", self.binary, message));
        }

        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Send a JSON-RPC notification (no response expected)
    fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });

        writeln!(self.stdin, "{}", notification)?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Call a tool on this server
    pub fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: Option<Value>,
    ) -> Result<CallToolResult> {
        let result = self.send_request_with_timeout(
            "tools/call",
            json!({
                "name": tool_name,
                "arguments": arguments.unwrap_or(Value::Null)
            }),
            TOOL_CALL_TIMEOUT,
        )?;

        // Parse the result into our CallToolResult type
        serde_json::from_value(result.clone()).with_context(|| {
            format!(
                "Failed to parse tool result from {}: {}",
                self.binary, result
            )
        })
    }

    /// Check if the child process is still running
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Convert external tool info to our ToolInfo type with namespaced name
    pub fn namespaced_tool(&self, tool: &ExternalToolInfo) -> ToolInfo {
        // Leak strings to get 'static lifetime - these live for program duration anyway
        // Use underscore separator - dots in tool names can cause issues with some MCP clients
        let name = Box::leak(format!("{}_{}", self.name, tool.name).into_boxed_str());
        let description = Box::leak(tool.description.clone().into_boxed_str());

        ToolInfo {
            name,
            description,
            input_schema: tool.input_schema.clone(),
            output_schema: tool.output_schema.clone(),
        }
    }

    /// Convert external resource info to our ResourceInfo type
    pub fn to_resource_info(&self, resource: &ExternalResourceInfo) -> ResourceInfo {
        ResourceInfo {
            uri: resource.uri.clone(),
            name: format!("{}_{}", self.name, resource.name),
            title: resource.title.clone(),
            description: resource.description.clone(),
            mime_type: resource.mime_type.clone(),
        }
    }
}

impl Drop for ExternalMcpServer {
    fn drop(&mut self) {
        // Try to kill the child process gracefully
        let _ = self.child.kill();
    }
}
