use crate::discovery::find_pcb_binaries;
use crate::proxy::ExternalMcpServer;
use crate::{CallToolResult, McpContext, ResourceInfo, ToolInfo};
use anyhow::Result;
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
        let mut aggregator =
            Self::new_without_discovery(builtin_tools, builtin_resources, builtin_handler);
        aggregator.discover_external_servers();
        aggregator
    }

    /// Create a new aggregator without auto-discovering external servers.
    /// Useful for testing or when external server discovery is not wanted.
    pub fn new_without_discovery(
        builtin_tools: Vec<ToolInfo>,
        builtin_resources: Vec<ResourceInfo>,
        builtin_handler: F,
    ) -> Self {
        Self {
            builtin_tools,
            builtin_resources,
            builtin_handler,
            external_servers: HashMap::new(),
        }
    }

    /// Discover and connect to external MCP servers
    fn discover_external_servers(&mut self) {
        for binary in find_pcb_binaries() {
            if let Ok(server) = ExternalMcpServer::spawn(&binary) {
                eprintln!(
                    "[pcb-mcp] Discovered: {} ({} tools, {} resources)",
                    server.name,
                    server.tools.len(),
                    server.resources.len()
                );
                self.external_servers.insert(server.name.clone(), server);
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
        // Namespaced tools use underscore separator: "namespace_toolname"
        // We split on the first underscore and check if the prefix is a known namespace
        if let Some((potential_namespace, tool_name)) = name.split_once('_')
            && self.external_servers.contains_key(potential_namespace)
        {
            let server = self.external_servers.get_mut(potential_namespace).unwrap();
            return server.call_tool(tool_name, args);
        }

        // Built-in tool (either no underscore, or prefix wasn't a known namespace)
        (self.builtin_handler)(name, args, ctx)
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
        // Use new_without_discovery to avoid spawning external servers in tests
        let aggregator = McpAggregator::new_without_discovery(vec![], vec![], dummy_handler);
        // Should not panic
        let _tools = aggregator.all_tools();
        let _resources = aggregator.all_resources();
    }
}
