//! Board quote API client
//!
//! Prices a board's working-tree IPC-2581 and design BOM the way the order
//! dialog prices a release: `POST /api/boards/{workspace}/{board}/quotes`.

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_ENCODING, CONTENT_TYPE};
use serde::Serialize;
use std::time::Duration;

use crate::WorkspaceContext;
use crate::release::{AUTHENTICATION_FAILED_MESSAGE, check_response};

/// IPC-2581 XML compresses about tenfold; boards run to tens of megabytes.
const ZSTD_LEVEL: i32 = 3;
/// The server runs DFM on the IPC-2581 when quoting US bare boards.
const QUOTE_TIMEOUT_SECS: u64 = 600;

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct QuoteRequest<'a> {
    /// `us`, `global-us`, or `global`
    pub route: &'a str,
    /// Assembled boards per quote
    pub quantities: &'a [u32],
    /// Extra unassembled boards in every quote
    pub bare_board_quantity: u32,
    pub ipc_xml: &'a str,
    /// Ungrouped design BOM entries, as `pcb_sch::bom::Bom::ungrouped_entries`
    pub design_bom: serde_json::Value,
}

/// Quote a board, returning the server's quote response JSON.
pub fn quote_board(
    ctx: &WorkspaceContext,
    workspace: &str,
    board: &str,
    request: &QuoteRequest,
) -> Result<serde_json::Value> {
    let token = crate::auth::get_api_token_with_context(ctx)?;
    send_quote(ctx, token.as_deref(), workspace, board, request)
}

fn send_quote(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    workspace: &str,
    board: &str,
    request: &QuoteRequest,
) -> Result<serde_json::Value> {
    let url = format!(
        "{}/api/boards/{}/{}/quotes",
        ctx.api_base_url().trim_end_matches('/'),
        urlencoding::encode(workspace),
        urlencoding::encode(board)
    );
    let body = zstd::encode_all(serde_json::to_vec(request)?.as_slice(), ZSTD_LEVEL)
        .context("Failed to compress quote request")?;
    let client = Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(QUOTE_TIMEOUT_SECS))
        .build()?;
    let response = crate::auth::apply_bearer_auth(client.post(&url), auth_token)
        .header(CONTENT_TYPE, "application/json")
        .header(CONTENT_ENCODING, "zstd")
        .body(body)
        .send()
        .context("Failed to connect to Diode API")?;
    let response = check_response(response, |status, message| match status {
        StatusCode::UNAUTHORIZED => AUTHENTICATION_FAILED_MESSAGE.into(),
        StatusCode::NOT_FOUND => format!(
            "Board '{workspace}/{board}' not found, or you can't quote it. The workspace comes \
             from [workspace].name or [workspace].repository in pcb.toml."
        ),
        _ => format!("Quote failed ({status}): {message}"),
    })?;
    response.json().context("Invalid quote response")
}

#[cfg(test)]
mod tests {
    use httpmock::{Method::POST, MockServer};

    use super::*;

    #[test]
    fn sends_a_zstd_compressed_request_to_the_board() {
        let server = MockServer::start();
        let request = QuoteRequest {
            route: "us",
            quantities: &[5, 10],
            bare_board_quantity: 2,
            ipc_xml: "<IPC-2581/>",
            design_bom: serde_json::json!([{ "designator": "R1", "mpn": "RC0402" }]),
        };
        let expected = serde_json::to_vec(&request).unwrap();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boards/acme/My%20Board/quotes")
                .header("content-encoding", "zstd")
                .header("content-type", "application/json")
                .is_true(move |req| zstd::decode_all(req.body().as_ref()).unwrap() == expected);
            then.status(200)
                .json_body(serde_json::json!({ "results": [] }));
        });
        let ctx = WorkspaceContext::from_api_base_url(server.base_url());

        let response = send_quote(&ctx, None, "acme", "My Board", &request).unwrap();

        mock.assert();
        assert_eq!(response, serde_json::json!({ "results": [] }));
    }
}
