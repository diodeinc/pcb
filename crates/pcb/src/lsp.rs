use clap::Args;

#[derive(Args)]
pub struct LspArgs {}

#[cfg(feature = "api")]
const RESOLVE_DATASHEET_METHOD: &str = "pcb/resolveDatasheet";

pub fn execute(_args: LspArgs) -> anyhow::Result<()> {
    #[cfg(feature = "api")]
    {
        let ctx = pcb_zen::lsp::LspEvalContext::default()
            .set_eager(false)
            .with_custom_request_handler(handle_custom_request);
        pcb_starlark_lsp::server::stdio_server(ctx)
    }

    #[cfg(not(feature = "api"))]
    {
        pcb_zen::lsp_with_eager(false)?;
        Ok(())
    }
}

#[cfg(feature = "api")]
fn handle_custom_request(
    method: &str,
    params: &serde_json::Value,
) -> anyhow::Result<Option<serde_json::Value>> {
    if method != RESOLVE_DATASHEET_METHOD {
        return Ok(None);
    }

    let input = pcb_diode_api::datasheet::parse_resolve_request(Some(params))?;
    let token = pcb_diode_api::auth::get_valid_token()?;
    let response = pcb_diode_api::datasheet::resolve_datasheet(&token, &input)?;
    Ok(Some(serde_json::to_value(response)?))
}

#[cfg(all(test, feature = "api"))]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn custom_request_handler_ignores_other_methods() {
        let result = handle_custom_request("pcb/somethingElse", &json!({})).unwrap();
        assert!(result.is_none());
    }
}
