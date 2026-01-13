use anyhow::Result;
use clap::Args;
use std::io::{self, IsTerminal};

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
    let content = if args.list {
        pcb_docs::lookup_list(&args.path)
    } else {
        pcb_docs::lookup(&args.path)
    };

    match content {
        Ok(content) => {
            // Use termimad for rich TTY rendering of doc content,
            // but print list output directly (termimad breaks nested list indentation)
            if !args.list && io::stdout().is_terminal() {
                termimad::print_text(&content);
            } else {
                println!("{}", content);
            }
            Ok(())
        }
        Err(e) => {
            anyhow::bail!("{}", e)
        }
    }
}
