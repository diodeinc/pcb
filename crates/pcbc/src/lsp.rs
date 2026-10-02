use clap::Args;

#[derive(Args)]
pub struct LspArgs {
    /// Disable network access; use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,
}

const RESOLVE_DATASHEET_METHOD: &str = "pcb/resolveDatasheet";

pub fn execute(args: LspArgs) -> anyhow::Result<()> {
    let offline = args.offline;
    pcb_zen::lsp_with_custom_request_handler(false, offline, move |method, params| {
        handle_custom_request(method, params, offline)
    })
}

fn handle_custom_request(
    method: &str,
    params: &serde_json::Value,
    offline: bool,
) -> anyhow::Result<Option<serde_json::Value>> {
    if method != RESOLVE_DATASHEET_METHOD {
        return Ok(None);
    }
    if offline {
        anyhow::bail!("{RESOLVE_DATASHEET_METHOD} is unavailable in offline mode");
    }

    let input = pcb_diode_api::datasheet::parse_resolve_request(Some(params))?;
    let ctx = pcb_diode_api::WorkspaceContext::from_cwd()?;
    let response = pcb_diode_api::datasheet::resolve_datasheet(&ctx, &input, None)?;
    Ok(Some(serde_json::to_value(response)?))
}
