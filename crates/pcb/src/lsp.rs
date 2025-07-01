use clap::Args;

#[derive(Args)]
pub struct LspArgs {}

pub fn execute(_args: LspArgs) -> anyhow::Result<()> {
    pcb_star::lsp_with_eager(true)?;
    Ok(())
}
