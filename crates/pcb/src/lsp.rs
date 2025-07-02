use clap::Args;

#[derive(Args)]
pub struct LspArgs {}

pub fn execute(_args: LspArgs) -> anyhow::Result<()> {
    pcb_zen::lsp_with_eager(true)?;
    Ok(())
}
