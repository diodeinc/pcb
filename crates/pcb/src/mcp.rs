use anyhow::Result;
use clap::Args;
use pcb_mcp::ResourceInfo;

#[derive(Args, Debug)]
pub struct McpArgs {}

pub fn execute(_args: McpArgs) -> Result<()> {
    let mut tools = vec![];

    #[cfg(feature = "api")]
    tools.extend(pcb_diode_api::mcp::tools());

    // Point to public Zener documentation (client fetches directly)
    let resources = vec![ResourceInfo {
        uri: "https://docs.pcb.new/pages/spec".to_string(),
        name: "spec".to_string(),
        title: "Zener Language Specification".to_string(),
        description: "Complete Zener HDL specification: core types, built-in functions, module system, type system, and examples.".to_string(),
        mime_type: "text/html".to_string(),
    }];

    pcb_mcp::run_server(&tools, &resources, |name, args, ctx| {
        #[cfg(feature = "api")]
        {
            pcb_diode_api::mcp::handle(name, args, ctx)
        }

        #[cfg(not(feature = "api"))]
        anyhow::bail!("Unknown tool: {}", name)
    })
}
