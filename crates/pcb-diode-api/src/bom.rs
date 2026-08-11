use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::{collections::HashMap, time::Duration};

use pcb_sch::bom::{Availability, AvailabilitySummary, BOARD_QUANTITY, Offer};

use crate::WorkspaceContext;

/// Price break structure
#[derive(Debug, Clone, Deserialize)]
struct PriceBreak {
    qty: i32,
    price: f64,
}

/// Geography/region for an offer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum Geography {
    Us,
    Uk,
    Global,
}

impl std::fmt::Display for Geography {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Us => "US",
            Self::Uk => "UK",
            Self::Global => "Global",
        })
    }
}

/// Component offer from API - internal deserialization type
#[derive(Debug, Clone, Deserialize)]
struct ComponentOffer {
    id: String,
    geography: Geography,
    distributor: Option<String>,
    #[serde(rename = "distributorPartId")]
    distributor_part_id: Option<String>,
    mpn: Option<String>,
    manufacturer: Option<String>,
    #[serde(rename = "priceBreaks")]
    price_breaks: Option<Vec<PriceBreak>>,
    #[serde(rename = "stockAvailable")]
    stock_available: Option<i32>,
    #[serde(rename = "productUrl")]
    product_url: Option<String>,
}

impl ComponentOffer {
    /// Calculate unit price at a given quantity using price breaks
    pub fn unit_price_at_qty(&self, qty: i32) -> Option<f64> {
        let breaks = self.price_breaks.as_ref().filter(|b| !b.is_empty())?;
        // Highest break <= qty, or lowest break if none apply
        breaks
            .iter()
            .filter(|pb| pb.qty <= qty)
            .max_by_key(|pb| pb.qty)
            .or_else(|| breaks.iter().min_by_key(|pb| pb.qty))
            .map(|pb| pb.price)
    }

    fn to_offer(&self, qty: i32) -> Offer {
        Offer {
            region: self.geography.to_string(),
            distributor: self.distributor.clone().unwrap_or_else(|| "—".into()),
            stock: self.stock_available.unwrap_or_default(),
            price: self.unit_price_at_qty(qty),
            part_id: self.distributor_part_id.clone(),
        }
    }
}

/// Design BOM entry structure from the API
#[derive(Debug, Deserialize)]
struct DesignBomEntry {
    path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
enum BomMatchStatus {
    #[serde(rename = "MATCH_EXACT")]
    Exact,
    #[serde(rename = "MATCH_COMPATIBLE")]
    Compatible,
    #[serde(rename = "MATCH_FUZZY")]
    Fuzzy,
    #[serde(rename = "MATCH_NEEDS_RETRY")]
    NeedsRetry,
    #[serde(rename = "MATCH_FAILED")]
    Failed,
}

/// BOM Line - represents a single line in the matched BOM response
#[derive(Debug, Deserialize)]
struct BomLine {
    #[serde(rename = "designEntry")]
    design_entry: DesignBomEntry,
    #[serde(rename = "offerIds")]
    offer_ids: Vec<String>,
    #[serde(rename = "match", default)]
    match_status: Option<BomMatchStatus>,
}

fn bom_line_no_match(bom_line: &BomLine) -> bool {
    matches!(bom_line.match_status, Some(BomMatchStatus::Failed))
}

/// Response from /api/boms/match endpoint
#[derive(Debug, Deserialize)]
struct MatchBomResponse {
    results: Vec<BomLine>,
    offers: HashMap<String, ComponentOffer>,
}

/// Calculate alt stock from offers, deduplicating by (distributor, mpn).
fn calculate_alt_stock(
    offers: &[&ComponentOffer],
    best_offer: Option<&ComponentOffer>,
    qty: i32,
) -> i32 {
    // Deduplicate by (distributor, mpn), keeping best price, excluding best_offer
    let mut best_by_key: HashMap<(&str, &str), &ComponentOffer> = HashMap::new();
    for o in offers
        .iter()
        .filter(|o| best_offer.is_none_or(|b| o.id != b.id))
    {
        let key = (
            o.distributor.as_deref().unwrap_or(""),
            o.mpn.as_deref().unwrap_or(""),
        );
        let dominated = best_by_key.get(&key).is_some_and(|existing| {
            o.unit_price_at_qty(qty).unwrap_or(f64::MAX)
                >= existing.unit_price_at_qty(qty).unwrap_or(f64::MAX)
        });
        if !dominated {
            best_by_key.insert(key, o);
        }
    }
    best_by_key.values().filter_map(|o| o.stock_available).sum()
}

/// Build AvailabilitySummary from an offer with alt stock total
fn build_availability_summary(
    offer: &ComponentOffer,
    alt_stock: i32,
    target_qty: i32,
) -> AvailabilitySummary {
    let lcsc_part_ids = match (offer.distributor.as_deref(), &offer.distributor_part_id) {
        (Some("lcsc"), Some(id)) => {
            let id = if id.starts_with('C') {
                id.clone()
            } else {
                format!("C{id}")
            };
            let url = offer
                .product_url
                .clone()
                .unwrap_or_else(|| format!("https://lcsc.com/product-detail/{id}.html"));
            vec![(id, url)]
        }
        _ => vec![],
    };

    AvailabilitySummary {
        price: offer.unit_price_at_qty(target_qty),
        stock: offer.stock_available.unwrap_or_default(),
        alt_stock,
        price_breaks: offer
            .price_breaks
            .as_ref()
            .map(|pbs| pbs.iter().map(|pb| (pb.qty, pb.price)).collect()),
        lcsc_part_ids,
        mpn: offer.mpn.clone().filter(|s| !s.is_empty()),
        manufacturer: offer.manufacturer.clone().filter(|s| !s.is_empty()),
    }
}

fn summarize_region<'a>(
    offers: &[&'a ComponentOffer],
    geography: Geography,
    target_qty: i32,
    alt_stock_price_qty: i32,
) -> (Vec<&'a ComponentOffer>, Option<AvailabilitySummary>) {
    let regional: Vec<_> = offers
        .iter()
        .copied()
        .filter(|offer| offer.geography == geography)
        .collect();
    let selected = regional.first().copied();
    let alt_stock = calculate_alt_stock(&regional, selected, alt_stock_price_qty);
    let summary = selected.map(|offer| build_availability_summary(offer, alt_stock, target_qty));
    (regional, summary)
}

/// Call the BOM match API and return parsed response
fn call_bom_match_api(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom_entries: &[serde_json::Value],
    timeout_secs: u64,
    strict: bool,
) -> Result<MatchBomResponse> {
    let url = bom_match_url(ctx.api_base_url(), strict);

    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;
    let request_body = serde_json::json!({
        "designBom": bom_entries,
        "format": "normalized",
        "boardQuantity": BOARD_QUANTITY,
        "regions": ["US", "GLOBAL"],
    });

    let response = crate::auth::apply_bearer_auth(client.post(&url), auth_token)
        .json(&request_body)
        .send()
        .context("Failed to send BOM match request")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().unwrap_or_default();
        anyhow::bail!("BOM match request failed ({}): {}", status, error_text);
    }

    response
        .json()
        .context("Failed to parse BOM match response")
}

fn bom_match_url(api_base_url: &str, strict: bool) -> String {
    let suffix = if strict { "?strict=true" } else { "" };
    format!("{api_base_url}/api/boms/match{suffix}")
}

/// Fetch BOM matching results from the API and populate availability data
pub fn fetch_and_populate_availability(
    auth_token: Option<&str>,
    bom: &mut pcb_sch::bom::Bom,
) -> Result<()> {
    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    fetch_and_populate_availability_with_context(&ctx, auth_token, bom, false)
}

pub fn fetch_and_populate_availability_with_context(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom: &mut pcb_sch::bom::Bom,
    strict: bool,
) -> Result<()> {
    let bom_json = bom.ungrouped_json();
    let bom_entries: Vec<serde_json::Value> =
        serde_json::from_str(&bom_json).context("Failed to parse BOM JSON")?;

    let match_response = call_bom_match_api(ctx, auth_token, &bom_entries, 120, strict)?;

    for bom_line in match_response.results {
        let Some(path) = bom_line.design_entry.path.as_deref() else {
            continue;
        };
        if !bom.entries.contains_key(path) {
            continue;
        }

        let qty = bom
            .designators
            .iter()
            .filter(|(p, _)| p.as_str() == path)
            .count() as i32;
        // Resolve offer IDs to actual offers from the deduplicated offers map
        let resolved_offers: Vec<&ComponentOffer> = bom_line
            .offer_ids
            .iter()
            .filter_map(|id| match_response.offers.get(id))
            .collect();

        let target_qty = qty * BOARD_QUANTITY;

        let (us_offers, us) = summarize_region(&resolved_offers, Geography::Us, target_qty, qty);
        let (global_offers, global) =
            summarize_region(&resolved_offers, Geography::Global, target_qty, qty);

        // Build offers for JSON output
        let all_offers: Vec<_> = us_offers
            .iter()
            .chain(global_offers.iter())
            .map(|o| o.to_offer(target_qty))
            .collect();

        bom.availability.insert(
            path.to_string(),
            Availability {
                us,
                global,
                no_match: bom_line_no_match(&bom_line),
                offers: all_offers,
            },
        );
    }

    Ok(())
}

/// Component key for pricing requests
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ComponentKey {
    pub mpn: String,
    pub manufacturer: Option<String>,
}

fn component_part_json(component: &ComponentKey) -> serde_json::Value {
    let mut part = serde_json::json!({ "mpn": component.mpn });
    if let Some(manufacturer) = &component.manufacturer {
        part["manufacturer"] = serde_json::json!(manufacturer);
    }
    part
}

fn component_bom_entry(index: usize, component: &ComponentKey) -> serde_json::Value {
    let mut entry = component_part_json(component);
    entry["path"] = serde_json::json!(format!("component_{index}"));
    entry["designator"] = serde_json::json!(format!("X{index}"));
    entry
}

fn grouped_component_bom_entry(
    index: usize,
    components: &[ComponentKey],
) -> Option<serde_json::Value> {
    let (primary, alternatives) = components.split_first()?;
    let mut entry = component_bom_entry(index, primary);
    entry["alternatives"] =
        serde_json::Value::Array(alternatives.iter().map(component_part_json).collect());
    Some(entry)
}

/// Format a price value for display (always 2 decimal places)
pub fn format_price(price: f64) -> String {
    format!("${:.2}", price)
}

/// Format a number with comma separators
pub fn format_number_with_commas(n: i32) -> String {
    n.to_string()
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| std::str::from_utf8(chunk).unwrap())
        .collect::<Vec<_>>()
        .join(",")
}

pub fn has_search_availability(availability: &Availability) -> bool {
    availability.us.is_some() || availability.global.is_some() || !availability.offers.is_empty()
}

/// Fetch pricing for multiple components in a single batch request
pub fn fetch_pricing_batch(
    auth_token: Option<&str>,
    components: &[ComponentKey],
) -> Result<Vec<Availability>> {
    fetch_pricing_batch_once(auth_token, components)
}

/// Fetch pricing for grouped alternate components as one planned BOM line per group.
pub fn fetch_pricing_grouped_batch(
    auth_token: Option<&str>,
    groups: &[Vec<ComponentKey>],
) -> Result<Vec<Availability>> {
    fetch_pricing_grouped_batch_once(auth_token, groups)
}

fn fetch_pricing_grouped_batch_once(
    auth_token: Option<&str>,
    groups: &[Vec<ComponentKey>],
) -> Result<Vec<Availability>> {
    if groups.is_empty() {
        return Ok(Vec::new());
    }

    let bom_entries: Vec<_> = groups
        .iter()
        .enumerate()
        .filter_map(|(index, group)| grouped_component_bom_entry(index, group))
        .collect();

    if bom_entries.is_empty() {
        return Ok(vec![Availability::default(); groups.len()]);
    }

    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    let match_response = call_bom_match_api(&ctx, auth_token, &bom_entries, 30, false)?;
    let mut results = vec![Availability::default(); groups.len()];

    for bom_line in &match_response.results {
        let Some(path) = bom_line.design_entry.path.as_deref() else {
            continue;
        };
        let Some(group_idx) = path
            .strip_prefix("component_")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            continue;
        };
        let Some(slot) = results.get_mut(group_idx) else {
            continue;
        };

        let offers: Vec<_> = bom_line
            .offer_ids
            .iter()
            .filter_map(|id| match_response.offers.get(id))
            .collect();
        *slot = build_search_availability(&offers, bom_line_no_match(bom_line));
    }

    Ok(results)
}

fn fetch_pricing_batch_once(
    auth_token: Option<&str>,
    components: &[ComponentKey],
) -> Result<Vec<Availability>> {
    if components.is_empty() {
        return Ok(Vec::new());
    }

    // Create BOM entries for all components
    let bom_entries: Vec<_> = components
        .iter()
        .enumerate()
        .map(|(index, component)| component_bom_entry(index, component))
        .collect();

    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    let match_response = call_bom_match_api(&ctx, auth_token, &bom_entries, 30, false)?;

    let mut results = vec![Availability::default(); components.len()];

    for bom_line in match_response.results {
        let Some(path) = bom_line.design_entry.path.as_deref() else {
            continue;
        };
        let Some(idx) = path
            .strip_prefix("component_")
            .and_then(|s| s.parse::<usize>().ok())
        else {
            continue;
        };
        let Some(slot) = results.get_mut(idx) else {
            continue;
        };

        let offers: Vec<_> = bom_line
            .offer_ids
            .iter()
            .filter_map(|id| match_response.offers.get(id))
            .collect();

        *slot = build_search_availability(&offers, bom_line_no_match(&bom_line));
    }

    Ok(results)
}

fn build_search_availability(offers: &[&ComponentOffer], no_match: bool) -> Availability {
    let (_, us) = summarize_region(offers, Geography::Us, 1, 1);
    let (_, global) = summarize_region(offers, Geography::Global, 1, 1);

    Availability {
        us,
        global,
        no_match,
        offers: offers
            .iter()
            .filter(|offer| offer.geography != Geography::Uk)
            .map(|offer| offer.to_offer(1))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(id: &str, breaks: &[(i32, f64)]) -> ComponentOffer {
        ComponentOffer {
            id: id.to_string(),
            geography: Geography::Us,
            distributor: Some("testdist".to_string()),
            distributor_part_id: Some(id.to_string()),
            mpn: Some("TEST-MPN".to_string()),
            manufacturer: Some("Test Manufacturer".to_string()),
            price_breaks: Some(
                breaks
                    .iter()
                    .map(|(qty, price)| PriceBreak {
                        qty: *qty,
                        price: *price,
                    })
                    .collect(),
            ),
            stock_available: Some(100),
            product_url: None,
        }
    }

    fn bom_line(match_status: Option<BomMatchStatus>, offer_ids: Vec<String>) -> BomLine {
        BomLine {
            design_entry: DesignBomEntry {
                path: Some("root.U1".to_string()),
            },
            offer_ids,
            match_status,
        }
    }

    #[test]
    fn match_status_controls_no_match_detection() {
        assert!(bom_line_no_match(&bom_line(
            Some(BomMatchStatus::Failed),
            vec!["offer-1".to_string()]
        )));

        for status in [
            BomMatchStatus::Exact,
            BomMatchStatus::Compatible,
            BomMatchStatus::Fuzzy,
            BomMatchStatus::NeedsRetry,
        ] {
            assert!(!bom_line_no_match(&bom_line(Some(status), Vec::new())));
        }
    }

    #[test]
    fn match_status_decodes_server_values() {
        for (json, expected) in [
            (r#""MATCH_EXACT""#, BomMatchStatus::Exact),
            (r#""MATCH_COMPATIBLE""#, BomMatchStatus::Compatible),
            (r#""MATCH_FUZZY""#, BomMatchStatus::Fuzzy),
            (r#""MATCH_NEEDS_RETRY""#, BomMatchStatus::NeedsRetry),
            (r#""MATCH_FAILED""#, BomMatchStatus::Failed),
        ] {
            assert_eq!(
                serde_json::from_str::<BomMatchStatus>(json).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn geography_decodes_server_values() {
        for (json, expected) in [
            (r#""US""#, Geography::Us),
            (r#""UK""#, Geography::Uk),
            (r#""GLOBAL""#, Geography::Global),
        ] {
            assert_eq!(serde_json::from_str::<Geography>(json).unwrap(), expected);
        }
    }

    #[test]
    fn uk_offers_are_ignored() {
        let mut uk_offer = offer("uk-offer", &[(1, 1.0)]);
        uk_offer.geography = Geography::Uk;

        let availability = build_search_availability(&[&uk_offer], false);

        assert!(availability.us.is_none());
        assert!(availability.global.is_none());
        assert!(availability.offers.is_empty());
    }

    #[test]
    fn missing_match_status_does_not_imply_no_match() {
        assert!(!bom_line_no_match(&bom_line(None, Vec::new())));
        assert!(!bom_line_no_match(&bom_line(
            None,
            vec!["offer-1".to_string()]
        )));
    }

    #[test]
    fn first_api_ranked_regional_offer_wins() {
        let response: MatchBomResponse =
            serde_json::from_str(include_str!("../tests/fixtures/bom_match_api_order.json"))
                .unwrap();
        let line = &response.results[0];
        let offers: Vec<_> = line
            .offer_ids
            .iter()
            .filter_map(|id| response.offers.get(id))
            .collect();

        let availability = build_search_availability(&offers, false);

        assert_eq!(availability.us.as_ref().unwrap().stock, 5);
        assert_eq!(availability.us.as_ref().unwrap().price, Some(10.0));
        assert_eq!(availability.offers[0].part_id.as_deref(), Some("API-FIRST"));
    }

    #[test]
    fn bom_match_url_selects_strict_matching_when_enabled() {
        assert_eq!(
            bom_match_url("https://api.diode.computer", false),
            "https://api.diode.computer/api/boms/match"
        );
        assert_eq!(
            bom_match_url("https://api.diode.computer", true),
            "https://api.diode.computer/api/boms/match?strict=true"
        );
    }
}
