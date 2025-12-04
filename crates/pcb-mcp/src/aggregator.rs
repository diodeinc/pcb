use crate::discovery::find_pcb_binaries;
use crate::proxy::ExternalMcpServer;
use crate::{CallToolResult, McpContext, ResourceInfo, ToolInfo};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;

/// Aggregates tools and resources from built-in handlers and external MCP servers
pub struct McpAggregator<F>
where
    F: Fn(&str, Option<Value>, &McpContext) -> Result<CallToolResult>,
{
    /// Built-in tools
    builtin_tools: Vec<ToolInfo>,
    /// Built-in resources
    builtin_resources: Vec<ResourceInfo>,
    /// Built-in tool handler
    builtin_handler: F,
    /// External MCP servers, keyed by namespace
    external_servers: HashMap<String, ExternalMcpServer>,
}

impl<F> McpAggregator<F>
where
    F: Fn(&str, Option<Value>, &McpContext) -> Result<CallToolResult>,
{
    /// Create a new aggregator with built-in tools and discover external servers
    pub fn new(
        builtin_tools: Vec<ToolInfo>,
        builtin_resources: Vec<ResourceInfo>,
        builtin_handler: F,
    ) -> Self {
        let mut aggregator = Self {
            builtin_tools,
            builtin_resources,
            builtin_handler,
            external_servers: HashMap::new(),
        };

        aggregator.discover_external_servers();
        aggregator
    }

    /// Discover and connect to external MCP servers
    fn discover_external_servers(&mut self) {
        let binaries = find_pcb_binaries();

        for binary in binaries {
            match ExternalMcpServer::spawn(&binary) {
                Ok(server) => {
                    let name = server.name.clone();
                    eprintln!(
                        "Discovered external MCP server: {} ({} tools, {} resources)",
                        name,
                        server.tools.len(),
                        server.resources.len()
                    );
                    self.external_servers.insert(name, server);
                }
                Err(_) => {
                    // Binary doesn't support MCP or failed to start - skip silently
                }
            }
        }
    }

    /// Get all tools (built-in + external, with namespacing)
    pub fn all_tools(&self) -> Vec<ToolInfo> {
        let mut tools = self.builtin_tools.clone();

        for server in self.external_servers.values() {
            for tool in &server.tools {
                tools.push(server.namespaced_tool(tool));
            }
        }

        tools
    }

    /// Get all resources (built-in + external)
    pub fn all_resources(&self) -> Vec<ResourceInfo> {
        let mut resources = self.builtin_resources.clone();

        for server in self.external_servers.values() {
            for resource in &server.resources {
                resources.push(server.to_resource_info(resource));
            }
        }

        resources
    }

    /// Handle a tool call, routing to the appropriate handler
    pub fn handle_tool_call(
        &mut self,
        name: &str,
        args: Option<Value>,
        ctx: &McpContext,
    ) -> Result<CallToolResult> {
        // Check if this is a namespaced tool (external)
        if let Some((namespace, tool_name)) = name.split_once(':') {
            let server = self
                .external_servers
                .get_mut(namespace)
                .ok_or_else(|| anyhow!("Unknown tool namespace: {}", namespace))?;

            server.call_tool(tool_name, args)
        } else {
            // Built-in tool
            (self.builtin_handler)(name, args, ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_handler(
        _name: &str,
        _args: Option<Value>,
        _ctx: &McpContext,
    ) -> Result<CallToolResult> {
        Ok(CallToolResult::error("Not implemented"))
    }

    #[test]
    fn test_aggregator_creation() {
        let aggregator = McpAggregator::new(vec![], vec![], dummy_handler);
        // Should not panic
        let _tools = aggregator.all_tools();
        let _resources = aggregator.all_resources();
    }
}
