use anyhow::Result;
use clap::Args;

#[derive(Args)]
pub struct DocArgs {
    /// Documentation path (page or page/section), e.g. "spec" or "spec/net"
    #[arg(default_value = "")]
    pub path: String,

    /// List available pages or sections instead of showing content
    #[arg(long, short = 'l')]
    pub list: bool,
}

pub fn execute(args: DocArgs) -> Result<()> {
    let output = if args.list {
        pcb_docs::lookup_list(&args.path)
    } else {
        pcb_docs::lookup(&args.path)
    };

    match output {
        Ok(content) => {
            println!("{}", content);
            Ok(())
        }
        Err(e) => {
            anyhow::bail!("{}", e)
        }
    }
}
