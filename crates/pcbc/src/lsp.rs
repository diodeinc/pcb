use clap::Args;

#[derive(Args)]
pub struct LspArgs {}

const RESOLVE_DATASHEET_METHOD: &str = "pcb/resolveDatasheet";

pub fn execute(_args: LspArgs) -> anyhow::Result<()> {
    pcb_zen::lsp_with_custom_request_handler(false, handle_custom_request)
}

fn handle_custom_request(
    method: &str,
    params: &serde_json::Value,
) -> anyhow::Result<Option<serde_json::Value>> {
    if method != RESOLVE_DATASHEET_METHOD {
        return Ok(None);
    }

    let input = pcb_diode_api::datasheet::parse_resolve_request(Some(params))?;
    let token = pcb_diode_api::auth::get_api_token()?;
    let response = pcb_diode_api::datasheet::resolve_datasheet(token.as_deref(), &input)?;
    Ok(Some(serde_json::to_value(response)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn custom_request_handler_ignores_other_methods() {
        let result = handle_custom_request("pcb/somethingElse", &json!({})).unwrap();
        assert!(result.is_none());
    }
}
