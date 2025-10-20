# pcb-mcp

Simple Model Context Protocol (MCP) server for PCB tools.

## Testing

```bash
# Ping
echo '{"jsonrpc":"2.0","id":1,"method":"ping"}' | pcb mcp | jq

# List tools
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | pcb mcp | jq

# Search components
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"search_component","arguments":{"part_number":"STM32"}}}' | pcb mcp | jq '.result.structuredContent.results[]'

# Add component (use component_id from search)
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"add_component","arguments":{"component_id":"<id>"}}}' | pcb mcp | jq
```

## Supported Methods

- `initialize` - Server handshake
- `ping` - Health check
- `logging/setLevel` - Set log level
- `tools/list` - List available tools (with output schemas)
- `tools/call` - Execute a tool
- Notifications (cancelled, etc.) - handled silently
