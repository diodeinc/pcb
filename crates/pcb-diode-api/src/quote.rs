//! Quote API client
//!
//! Prices a board's working-tree IPC-2581 and design BOM: `POST /api/quotes`.

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use serde::Serialize;
use std::time::Duration;

use crate::WorkspaceContext;
use crate::release::{AUTHENTICATION_FAILED_MESSAGE, check_response};

/// The server runs DFM on the IPC-2581 when quoting US bare boards.
const QUOTE_TIMEOUT_SECS: u64 = 600;

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuoteRequest<'a> {
    /// `us` or `global-us`
    pub route: &'a str,
    /// Assembled boards per quote
    pub quantities: &'a [u32],
    /// Extra unassembled boards in every quote
    pub bare_board_quantity: u32,
    pub ipc_xml: &'a str,
    /// Ungrouped design BOM entries, as `pcb_sch::bom::Bom::ungrouped_entries`
    pub design_bom: serde_json::Value,
}

/// Quote a board's artifacts, returning the server's quote response JSON.
pub fn quote(ctx: &WorkspaceContext, request: &QuoteRequest) -> Result<serde_json::Value> {
    let token = crate::auth::get_api_token_with_context(ctx)?;
    send_quote(ctx, token.as_deref(), request)
}

fn send_quote(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    request: &QuoteRequest,
) -> Result<serde_json::Value> {
    let url = format!("{}/api/quotes", ctx.api_base_url().trim_end_matches('/'));
    let client = Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(QUOTE_TIMEOUT_SECS))
        .build()?;
    let response = crate::auth::apply_bearer_auth(client.post(&url), auth_token)
        .json(request)
        .send()
        .context("Failed to connect to Diode API")?;
    let response = check_response(response, |status, message| match status {
        StatusCode::UNAUTHORIZED => AUTHENTICATION_FAILED_MESSAGE.into(),
        _ => format!("Quote failed ({status}): {message}"),
    })?;
    response.json().context("Invalid quote response")
}

#[cfg(test)]
mod tests {
    use httpmock::{Method::POST, MockServer};

    use super::*;

    #[test]
    fn posts_the_quote_request() {
        let server = MockServer::start();
        let request = QuoteRequest {
            route: "us",
            quantities: &[5, 10],
            bare_board_quantity: 2,
            ipc_xml: "<IPC-2581/>",
            design_bom: serde_json::json!([{ "designator": "R1", "mpn": "RC0402" }]),
        };
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/quotes")
                .json_body(serde_json::json!({
                    "route": "us",
                    "quantities": [5, 10],
                    "bareBoardQuantity": 2,
                    "ipcXml": "<IPC-2581/>",
                    "designBom": [{ "designator": "R1", "mpn": "RC0402" }],
                }));
            then.status(200)
                .json_body(serde_json::json!({ "results": [] }));
        });
        let ctx = WorkspaceContext::from_api_base_url(server.base_url());

        let response = send_quote(&ctx, None, &request).unwrap();

        mock.assert();
        assert_eq!(response, serde_json::json!({ "results": [] }));
    }
}
