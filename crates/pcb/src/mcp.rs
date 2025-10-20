use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct McpArgs {}

pub fn execute(_args: McpArgs) -> Result<()> {
    let mut tools = vec![];

    #[cfg(feature = "api")]
    tools.extend(pcb_diode_api::mcp::tools());

    pcb_mcp::run_server(&tools, |name, args, ctx| {
        #[cfg(feature = "api")]
        if let Ok(result) = pcb_diode_api::mcp::handle(name, args, ctx) {
            return Ok(result);
        }

        anyhow::bail!("Unknown tool: {}", name)
    })
}
