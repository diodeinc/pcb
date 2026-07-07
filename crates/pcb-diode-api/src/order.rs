//! Read-only `pcb order` command group.
//!
//! Three subcommands, all backed by existing authenticated API endpoints:
//!
//! * `pcb order list`            -> `GET /api/boards/:workspace/:name/orders`
//! * `pcb order show <id>`       -> `GET /api/boards/:workspace/:name/orders/:orderId`
//! * `pcb order bom  <id>`       -> `GET /api/boms/:bomId` joined with
//!   `GET /api/boards/:workspace/:name/orders/:orderId/selections`
//!
//! Board identity is resolved from the workspace config (`workspace.repository`,
//! e.g. `code.diode.computer/demo/b/DM0002` -> workspace `demo`, board `DM0002`),
//! and can be overridden with `--workspace`/`--board`.
//!
//! All subcommands are strictly read-only. Order-mutating flows (create, select)
//! deliberately live in a separate change.

use anyhow::{Result, anyhow, bail};
use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;
use comfy_table::{Cell, ContentArrangement, Table, presets};
use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::auth::get_valid_token;
use crate::get_api_base_url;

// ---------------------------------------------------------------------------
// Board identity resolution
// ---------------------------------------------------------------------------

/// A resolved (`workspace`, `board`) pair used to build API paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardIdentity {
    pub workspace: String,
    pub board: String,
}

/// Parse a workspace `repository` string into `(workspace, board)`.
///
/// The board repository format is `<host>/<workspace>/b/<board>`, e.g.
/// `code.diode.computer/demo/b/DM0002` -> `("demo", "DM0002")`. We anchor on the
/// `b` marker so that hosts with extra path segments still resolve correctly.
///
/// Returns `None` when the string does not describe a board repository.
pub fn parse_board_repository(repository: &str) -> Option<(String, String)> {
    let segments: Vec<&str> = repository.split('/').filter(|s| !s.is_empty()).collect();
    let b_idx = segments.iter().position(|s| *s == "b")?;
    // Need a segment before ("workspace") and after ("board") the `b` marker.
    if b_idx == 0 {
        return None;
    }
    let workspace = *segments.get(b_idx - 1)?;
    let board = *segments.get(b_idx + 1)?;
    if workspace.is_empty() || board.is_empty() {
        return None;
    }
    Some((workspace.to_string(), board.to_string()))
}

/// Resolve the board identity from the workspace repository plus optional
/// `--workspace`/`--board` overrides.
///
/// Flags take precedence over the inferred values. When neither flags nor the
/// workspace repository provide a value, we fail with a clear, actionable error.
pub fn resolve_board_identity(
    repository: Option<&str>,
    workspace_flag: Option<&str>,
    board_flag: Option<&str>,
) -> Result<BoardIdentity> {
    let parsed = repository.and_then(parse_board_repository);

    let workspace = workspace_flag
        .map(str::to_string)
        .or_else(|| parsed.as_ref().map(|(w, _)| w.clone()));
    let board = board_flag
        .map(str::to_string)
        .or_else(|| parsed.as_ref().map(|(_, b)| b.clone()));

    match (workspace, board) {
        (Some(workspace), Some(board)) => Ok(BoardIdentity { workspace, board }),
        _ => bail!(
            "Could not determine which board to use.\n\
             Run this command from inside a board workspace (see `pcb info`), \
             or pass both --workspace <slug> and --board <name>."
        ),
    }
}

/// Read the current workspace repository (if any) starting from the CWD.
///
/// Any discovery error is swallowed to `None` so that explicit
/// `--workspace`/`--board` flags keep working outside a workspace.
fn current_workspace_repository() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let file_provider = pcb_zen_core::DefaultFileProvider::new();
    let ws = pcb_zen::get_workspace_info(&file_provider, &cwd).ok()?;
    ws.repository().map(|s| s.to_string())
}

// ---------------------------------------------------------------------------
// MPN normalization (mirrors backend `normalizeBomLookupMpn`)
// ---------------------------------------------------------------------------

/// Normalize an MPN for comparison: trim, uppercase, then strip every character
/// except `A-Z`, `0-9`, and `.`.
///
/// This mirrors the backend's `normalizeBomLookupMpn` so that mismatch detection
/// on the client agrees with server-side matching.
pub fn normalize_bom_lookup_mpn(mpn: &str) -> String {
    mpn.trim()
        .to_uppercase()
        .chars()
        .filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '.')
        .collect()
}

// ---------------------------------------------------------------------------
// API response models
// ---------------------------------------------------------------------------

/// A single order as returned by the list endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderSummary {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub quantity: Option<i64>,
    #[serde(default)]
    pub release_version: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

/// A quote summary attached to an order, when present.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuoteSummary {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub unit_price: Option<f64>,
    #[serde(default)]
    pub total: Option<f64>,
    #[serde(default)]
    pub lead_time_days: Option<i64>,
}

/// A single entry in an order's status timeline.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// The full order detail as returned by the show endpoint.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderDetail {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub quantity: Option<i64>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub release_id: Option<String>,
    #[serde(default)]
    pub release_version: Option<String>,
    #[serde(default)]
    pub bom_id: Option<String>,
    #[serde(default)]
    pub quote: Option<QuoteSummary>,
    #[serde(default)]
    pub timeline: Vec<TimelineEntry>,
    #[serde(default)]
    pub shipping_location_id: Option<String>,
}

/// A declared alternative part on a BOM line.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Alternative {
    #[serde(default)]
    pub mpn: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
}

/// A candidate distributor offer for a BOM line.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Offer {
    pub id: String,
    #[serde(default)]
    pub mpn: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub distributor: Option<String>,
    #[serde(default)]
    pub stock: Option<i64>,
    #[serde(default)]
    pub price: Option<f64>,
}

/// A single line of a BOM as returned by `GET /api/boms/:bomId`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BomLine {
    pub id: String,
    #[serde(default)]
    pub mpn: Option<String>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub designator: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub alternatives: Vec<Alternative>,
    #[serde(default)]
    pub match_status: Option<String>,
    #[serde(default)]
    pub offers: Vec<Offer>,
    #[serde(default)]
    pub selected_offer_id: Option<String>,
}

/// The BOM document returned by `GET /api/boms/:bomId`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bom {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub lines: Vec<BomLine>,
}

// ---------------------------------------------------------------------------
// Joined `order bom` output model
// ---------------------------------------------------------------------------

/// Where the effective selection for a BOM line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    /// The order's selections map overrode the line default.
    OrderOverride,
    /// The line's own `selectedOfferId` default was used.
    Default,
    /// Neither an override nor a default was present.
    None,
}

/// The design-side view of a BOM line.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignEntry {
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub package: Option<String>,
    pub value: Option<String>,
    pub designator: Option<String>,
    pub path: Option<String>,
    pub alternatives: Vec<Alternative>,
}

/// One fully joined BOM line: design entry + candidate offers + effective selection.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderBomRow {
    pub bom_line_id: String,
    pub design: DesignEntry,
    pub match_status: Option<String>,
    pub offers: Vec<Offer>,
    pub selected_offer_id: Option<String>,
    pub selection_source: SelectionSource,
    pub selected_mpn: Option<String>,
    pub selected_manufacturer: Option<String>,
}

impl OrderBomRow {
    /// True when both the design MPN and selected MPN are present and differ
    /// under [`normalize_bom_lookup_mpn`]. Lines with no selection or no design
    /// MPN are never mismatches.
    pub fn is_mpn_mismatch(&self) -> bool {
        match (self.design.mpn.as_deref(), self.selected_mpn.as_deref()) {
            (Some(design), Some(selected)) => {
                normalize_bom_lookup_mpn(design) != normalize_bom_lookup_mpn(selected)
            }
            _ => false,
        }
    }
}

/// The full `order bom` report, serialized in JSON mode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderBomReport {
    pub order_id: String,
    pub bom_id: String,
    pub rows: Vec<OrderBomRow>,
}

/// Resolve the BOM id an order points at, failing clearly when the order has no BOM.
pub fn resolve_order_bom_id(order: &OrderDetail) -> Result<&str> {
    order
        .bom_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("Order '{}' has no BOM.", order.id))
}

/// Join a BOM with the order's selections map into per-line rows.
///
/// Effective selection precedence per line:
/// 1. `selections[bomLineId]` (order override), else
/// 2. `line.selectedOfferId` (default), else
/// 3. none.
///
/// This is a pure function so the join precedence can be tested directly.
pub fn build_order_bom_rows(bom: &Bom, selections: &BTreeMap<String, String>) -> Vec<OrderBomRow> {
    bom.lines
        .iter()
        .map(|line| {
            let (selected_offer_id, selection_source) = match selections.get(&line.id) {
                Some(offer_id) => (Some(offer_id.clone()), SelectionSource::OrderOverride),
                None => match &line.selected_offer_id {
                    Some(offer_id) => (Some(offer_id.clone()), SelectionSource::Default),
                    None => (None, SelectionSource::None),
                },
            };

            // Resolve the selected offer (if any) to convenience MPN/manufacturer.
            let selected_offer = selected_offer_id
                .as_deref()
                .and_then(|id| line.offers.iter().find(|o| o.id == id));

            OrderBomRow {
                bom_line_id: line.id.clone(),
                design: DesignEntry {
                    mpn: line.mpn.clone(),
                    manufacturer: line.manufacturer.clone(),
                    package: line.package.clone(),
                    value: line.value.clone(),
                    designator: line.designator.clone(),
                    path: line.path.clone(),
                    alternatives: line.alternatives.clone(),
                },
                match_status: line.match_status.clone(),
                offers: line.offers.clone(),
                selected_mpn: selected_offer.and_then(|o| o.mpn.clone()),
                selected_manufacturer: selected_offer.and_then(|o| o.manufacturer.clone()),
                selected_offer_id,
                selection_source,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// HTTP client + error handling
// ---------------------------------------------------------------------------

fn create_client() -> Result<Client> {
    Client::builder()
        .user_agent(format!("diode-pcb/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| anyhow!("Failed to create HTTP client: {e}"))
}

/// Describes what a request was fetching, so 404s can be disambiguated.
struct RequestTarget<'a> {
    workspace: &'a str,
    board: &'a str,
    order_id: Option<&'a str>,
    bom_id: Option<&'a str>,
}

/// Extract a human-readable `error` message from a JSON error body, if any.
fn extract_error_message(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("error")?
        .as_str()
        .map(String::from)
}

/// Map a non-success HTTP response into a uniform, actionable error.
fn map_error_response(resp: Response, target: &RequestTarget) -> anyhow::Error {
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    let msg = extract_error_message(&body).unwrap_or_default();

    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            anyhow!("Not authorized. Run `pcb auth login` to authenticate.")
        }
        StatusCode::NOT_FOUND => {
            let lower = msg.to_lowercase();
            if let Some(bom_id) = target.bom_id
                && lower.contains("bom")
            {
                return anyhow!("BOM '{bom_id}' not found.");
            }
            match target.order_id {
                // Order-scoped request: could be an unknown board or unknown order.
                Some(order_id) => {
                    if lower.contains("board") {
                        anyhow!(
                            "Board '{}' not found in workspace '{}'.",
                            target.board,
                            target.workspace
                        )
                    } else {
                        anyhow!(
                            "Order '{order_id}' not found for board '{}' in workspace '{}'.",
                            target.board,
                            target.workspace
                        )
                    }
                }
                // Board-scoped request: the board must be unknown.
                None => anyhow!(
                    "Board '{}' not found in workspace '{}'.",
                    target.board,
                    target.workspace
                ),
            }
        }
        other => {
            if msg.is_empty() {
                anyhow!("Diode API request failed ({other}).")
            } else {
                anyhow!("Diode API request failed ({other}): {msg}")
            }
        }
    }
}

/// Perform a GET request and return the response body as text, mapping errors.
fn get_text(client: &Client, token: &str, url: &str, target: &RequestTarget) -> Result<String> {
    let resp = client
        .get(url)
        .bearer_auth(token)
        .send()
        .map_err(|e| anyhow!("Network error contacting Diode API: {e}"))?;

    if !resp.status().is_success() {
        return Err(map_error_response(resp, target));
    }

    resp.text()
        .map_err(|e| anyhow!("Failed to read Diode API response: {e}"))
}

/// Fetch the orders list for a board.
pub fn fetch_orders(
    client: &Client,
    token: &str,
    identity: &BoardIdentity,
) -> Result<Vec<OrderSummary>> {
    let url = format!(
        "{}/api/boards/{}/{}/orders",
        get_api_base_url(),
        urlencoding::encode(&identity.workspace),
        urlencoding::encode(&identity.board),
    );
    let target = RequestTarget {
        workspace: &identity.workspace,
        board: &identity.board,
        order_id: None,
        bom_id: None,
    };
    let body = get_text(client, token, &url, &target)?;
    parse_orders_list(&body)
}

/// Fetch a single order's detail.
pub fn fetch_order(
    client: &Client,
    token: &str,
    identity: &BoardIdentity,
    order_id: &str,
) -> Result<OrderDetail> {
    let url = format!(
        "{}/api/boards/{}/{}/orders/{}",
        get_api_base_url(),
        urlencoding::encode(&identity.workspace),
        urlencoding::encode(&identity.board),
        urlencoding::encode(order_id),
    );
    let target = RequestTarget {
        workspace: &identity.workspace,
        board: &identity.board,
        order_id: Some(order_id),
        bom_id: None,
    };
    let body = get_text(client, token, &url, &target)?;
    serde_json::from_str(&body).map_err(|e| anyhow!("Failed to parse order response: {e}"))
}

/// Fetch a BOM document by id.
pub fn fetch_bom(
    client: &Client,
    token: &str,
    identity: &BoardIdentity,
    order_id: &str,
    bom_id: &str,
) -> Result<Bom> {
    let url = format!(
        "{}/api/boms/{}",
        get_api_base_url(),
        urlencoding::encode(bom_id),
    );
    let target = RequestTarget {
        workspace: &identity.workspace,
        board: &identity.board,
        order_id: Some(order_id),
        bom_id: Some(bom_id),
    };
    let body = get_text(client, token, &url, &target)?;
    serde_json::from_str(&body).map_err(|e| anyhow!("Failed to parse BOM response: {e}"))
}

/// Fetch the order's selections map (`{bomLineId -> offerId}`).
pub fn fetch_selections(
    client: &Client,
    token: &str,
    identity: &BoardIdentity,
    order_id: &str,
) -> Result<BTreeMap<String, String>> {
    let url = format!(
        "{}/api/boards/{}/{}/orders/{}/selections",
        get_api_base_url(),
        urlencoding::encode(&identity.workspace),
        urlencoding::encode(&identity.board),
        urlencoding::encode(order_id),
    );
    let target = RequestTarget {
        workspace: &identity.workspace,
        board: &identity.board,
        order_id: Some(order_id),
        bom_id: None,
    };
    let body = get_text(client, token, &url, &target)?;
    parse_selections(&body)
}

/// Parse the orders list response, tolerating either a bare array or an object
/// with an `orders` array.
fn parse_orders_list(body: &str) -> Result<Vec<OrderSummary>> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| anyhow!("Failed to parse orders response: {e}"))?;
    let array = if value.is_array() {
        value
    } else {
        value
            .get("orders")
            .cloned()
            .ok_or_else(|| anyhow!("Unexpected orders response shape"))?
    };
    serde_json::from_value(array).map_err(|e| anyhow!("Failed to parse orders response: {e}"))
}

/// Parse the selections response, tolerating either a bare map or an object with
/// a `selections` map.
fn parse_selections(body: &str) -> Result<BTreeMap<String, String>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| anyhow!("Failed to parse selections response: {e}"))?;
    let map = value.get("selections").cloned().unwrap_or(value);
    if map.is_null() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_value(map).map_err(|e| anyhow!("Failed to parse selections response: {e}"))
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

const NONE: &str = "—";

fn opt(value: &Option<String>) -> &str {
    value.as_deref().filter(|s| !s.is_empty()).unwrap_or(NONE)
}

fn opt_i64(value: Option<i64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_else(|| NONE.into())
}

fn fmt_price(currency: Option<&str>, price: Option<f64>) -> String {
    match price {
        Some(p) => match currency {
            Some(c) if !c.is_empty() => format!("{p:.2} {c}"),
            _ => format!("${p:.2}"),
        },
        None => NONE.into(),
    }
}

/// Render a date-ish string, trimming an ISO timestamp to `YYYY-MM-DD HH:MM` when possible.
fn fmt_date(value: &Option<String>) -> String {
    let Some(raw) = value.as_deref().filter(|s| !s.is_empty()) else {
        return NONE.into();
    };
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => raw.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

fn render_orders_table(orders: &[OrderSummary]) {
    if orders.is_empty() {
        println!("No orders found.");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(vec![
        Cell::new("ID"),
        Cell::new("Name"),
        Cell::new("Status"),
        Cell::new("Qty"),
        Cell::new("Release"),
        Cell::new("Provider"),
        Cell::new("Created"),
    ]);

    for o in orders {
        table.add_row(vec![
            Cell::new(&o.id),
            Cell::new(opt(&o.name)),
            Cell::new(opt(&o.status)),
            Cell::new(opt_i64(o.quantity)),
            Cell::new(opt(&o.release_version)),
            Cell::new(opt(&o.provider)),
            Cell::new(fmt_date(&o.created_at)),
        ]);
    }

    println!("{table}");
}

fn render_order_detail(order: &OrderDetail) {
    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_BORDERS_ONLY)
        .set_content_arrangement(ContentArrangement::Dynamic);

    table.add_row(vec![Cell::new("ID"), Cell::new(&order.id)]);
    table.add_row(vec![Cell::new("Name"), Cell::new(opt(&order.name))]);
    table.add_row(vec![Cell::new("Status"), Cell::new(opt(&order.status))]);
    table.add_row(vec![
        Cell::new("Quantity"),
        Cell::new(opt_i64(order.quantity)),
    ]);
    table.add_row(vec![Cell::new("Provider"), Cell::new(opt(&order.provider))]);
    table.add_row(vec![
        Cell::new("Created"),
        Cell::new(fmt_date(&order.created_at)),
    ]);
    table.add_row(vec![
        Cell::new("Release ID"),
        Cell::new(opt(&order.release_id)),
    ]);
    table.add_row(vec![
        Cell::new("Release Version"),
        Cell::new(opt(&order.release_version)),
    ]);
    table.add_row(vec![Cell::new("BOM ID"), Cell::new(opt(&order.bom_id))]);
    table.add_row(vec![
        Cell::new("Shipping Location ID"),
        Cell::new(opt(&order.shipping_location_id)),
    ]);

    println!("{}", "Order".bold());
    println!("{table}");

    if let Some(quote) = &order.quote {
        println!();
        println!("{}", "Quote".bold());
        let mut q = Table::new();
        q.load_preset(presets::UTF8_BORDERS_ONLY)
            .set_content_arrangement(ContentArrangement::Dynamic);
        q.add_row(vec![Cell::new("Quote ID"), Cell::new(opt(&quote.id))]);
        q.add_row(vec![Cell::new("Status"), Cell::new(opt(&quote.status))]);
        q.add_row(vec![
            Cell::new("Unit Price"),
            Cell::new(fmt_price(quote.currency.as_deref(), quote.unit_price)),
        ]);
        q.add_row(vec![
            Cell::new("Total"),
            Cell::new(fmt_price(quote.currency.as_deref(), quote.total)),
        ]);
        q.add_row(vec![
            Cell::new("Lead Time (days)"),
            Cell::new(opt_i64(quote.lead_time_days)),
        ]);
        println!("{q}");
    }

    if !order.timeline.is_empty() {
        println!();
        println!("{}", "Timeline".bold());
        let mut t = Table::new();
        t.load_preset(presets::UTF8_FULL_CONDENSED)
            .set_content_arrangement(ContentArrangement::Dynamic);
        t.set_header(vec![
            Cell::new("Status"),
            Cell::new("When"),
            Cell::new("Note"),
        ]);
        for entry in &order.timeline {
            t.add_row(vec![
                Cell::new(opt(&entry.status)),
                Cell::new(fmt_date(&entry.timestamp)),
                Cell::new(opt(&entry.note)),
            ]);
        }
        println!("{t}");
    }
}

fn render_order_bom_table(report: &OrderBomReport) {
    if report.rows.is_empty() {
        println!("No BOM lines found.");
        return;
    }

    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic);
    // Compact row: full offer detail is reserved for JSON mode.
    table.set_header(vec![
        Cell::new("Designator"),
        Cell::new("Design MPN"),
        Cell::new("Selected MPN"),
        Cell::new("Match"),
        Cell::new("Selection"),
    ]);

    for row in &report.rows {
        let designator = row
            .design
            .designator
            .clone()
            .or_else(|| row.design.path.clone());
        let selection = match row.selection_source {
            SelectionSource::OrderOverride => "order_override",
            SelectionSource::Default => "default",
            SelectionSource::None => "none",
        };
        table.add_row(vec![
            Cell::new(opt(&designator)),
            Cell::new(opt(&row.design.mpn)),
            Cell::new(opt(&row.selected_mpn)),
            Cell::new(opt(&row.match_status)),
            Cell::new(selection),
        ]);
    }

    println!("{table}");
}

fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// Output format for `pcb order` subcommands.
#[derive(ValueEnum, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OrderFormat {
    /// Human-readable table (default).
    #[default]
    Table,
    /// Machine-readable JSON.
    Json,
}

impl std::fmt::Display for OrderFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderFormat::Table => write!(f, "table"),
            OrderFormat::Json => write!(f, "json"),
        }
    }
}

/// Board selection flags shared by all `pcb order` subcommands.
#[derive(Args, Debug, Clone)]
pub struct BoardSelector {
    /// Workspace slug (overrides the workspace inferred from pcb.toml)
    #[arg(long, value_name = "SLUG")]
    pub workspace: Option<String>,

    /// Board name (overrides the board inferred from pcb.toml)
    #[arg(long, value_name = "NAME")]
    pub board: Option<String>,
}

impl BoardSelector {
    fn resolve(&self) -> Result<BoardIdentity> {
        let repository = current_workspace_repository();
        resolve_board_identity(
            repository.as_deref(),
            self.workspace.as_deref(),
            self.board.as_deref(),
        )
    }
}

#[derive(Args, Debug)]
#[command(about = "Inspect fabrication orders for a board (read-only)")]
pub struct OrderArgs {
    #[command(subcommand)]
    pub command: OrderCommand,
}

#[derive(Subcommand, Debug)]
pub enum OrderCommand {
    /// List orders for a board
    List(OrderListArgs),
    /// Show a single order by id
    Show(OrderShowArgs),
    /// Show the resolved BOM (with selections) for an order
    Bom(OrderBomArgs),
}

#[derive(Args, Debug)]
pub struct OrderListArgs {
    #[command(flatten)]
    pub board: BoardSelector,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = OrderFormat::Table)]
    pub format: OrderFormat,
}

#[derive(Args, Debug)]
pub struct OrderShowArgs {
    /// Order id
    #[arg(value_name = "ORDER_ID")]
    pub order_id: String,

    #[command(flatten)]
    pub board: BoardSelector,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = OrderFormat::Table)]
    pub format: OrderFormat,
}

#[derive(Args, Debug)]
pub struct OrderBomArgs {
    /// Order id
    #[arg(value_name = "ORDER_ID")]
    pub order_id: String,

    #[command(flatten)]
    pub board: BoardSelector,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = OrderFormat::Table)]
    pub format: OrderFormat,

    /// Only show lines where the selected MPN differs from the design MPN
    #[arg(long)]
    pub mismatches_only: bool,
}

// ---------------------------------------------------------------------------
// Command entry points
// ---------------------------------------------------------------------------

pub fn execute(args: OrderArgs) -> Result<()> {
    match args.command {
        OrderCommand::List(args) => execute_list(args),
        OrderCommand::Show(args) => execute_show(args),
        OrderCommand::Bom(args) => execute_bom(args),
    }
}

fn execute_list(args: OrderListArgs) -> Result<()> {
    let identity = args.board.resolve()?;
    let token = get_valid_token()?;
    let client = create_client()?;

    let orders = fetch_orders(&client, &token, &identity)?;

    match args.format {
        OrderFormat::Table => render_orders_table(&orders),
        OrderFormat::Json => print_json(&orders)?,
    }
    Ok(())
}

fn execute_show(args: OrderShowArgs) -> Result<()> {
    let identity = args.board.resolve()?;
    let token = get_valid_token()?;
    let client = create_client()?;

    let order = fetch_order(&client, &token, &identity, &args.order_id)?;

    match args.format {
        OrderFormat::Table => render_order_detail(&order),
        OrderFormat::Json => print_json(&order)?,
    }
    Ok(())
}

fn execute_bom(args: OrderBomArgs) -> Result<()> {
    let identity = args.board.resolve()?;
    let token = get_valid_token()?;
    let client = create_client()?;

    let order = fetch_order(&client, &token, &identity, &args.order_id)?;
    let bom_id = resolve_order_bom_id(&order)?;

    let bom = fetch_bom(&client, &token, &identity, &args.order_id, bom_id)?;
    let selections = fetch_selections(&client, &token, &identity, &args.order_id)?;

    let mut rows = build_order_bom_rows(&bom, &selections);
    if args.mismatches_only {
        rows.retain(OrderBomRow::is_mpn_mismatch);
    }

    let report = OrderBomReport {
        order_id: args.order_id.clone(),
        bom_id: bom_id.to_string(),
        rows,
    };

    match args.format {
        OrderFormat::Table => render_order_bom_table(&report),
        OrderFormat::Json => print_json(&report)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests;
