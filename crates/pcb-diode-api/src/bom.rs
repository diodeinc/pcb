use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use pcb_sch::bom::{
    Availability, AvailabilitySummary, BOARD_QUANTITY, BomEntry, BomMatchStatus, Offer,
    PartCollection, SourcingStockClass,
};

use crate::WorkspaceContext;

const BOM_MATCH_TIMEOUT_SECS: u64 = 120;

/// Price break structure
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PriceBreak {
    qty: i32,
    price: f64,
}

/// Geography/region for an offer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum Geography {
    Us,
    Global,
    /// Offers outside the sourcing regions are discovered but never ranked.
    #[serde(other)]
    Other,
}

impl std::fmt::Display for Geography {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Us => "US",
            Self::Global => "Global",
            Self::Other => "Other",
        })
    }
}

/// Component offer from API - internal deserialization type
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(rename = "datasheetUrl", skip_serializing_if = "Option::is_none")]
    datasheet_url: Option<String>,
    #[serde(rename = "partCollections", default)]
    part_collections: Vec<PartCollection>,
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
            id: Some(self.id.clone()),
            region: self.geography.to_string(),
            distributor: self.distributor.clone().unwrap_or_else(|| "—".into()),
            stock: self.stock_available.unwrap_or_default(),
            price: self.unit_price_at_qty(qty),
            part_id: self.distributor_part_id.clone(),
            mpn: self.mpn.clone(),
            manufacturer: self.manufacturer.clone(),
            datasheet_url: self.datasheet_url.clone(),
            part_collections: self.part_collections.clone(),
        }
    }
}

/// Design BOM entry structure from the API
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DesignBomEntry {
    path: Option<String>,
}

/// BOM Line - represents a single line in the matched BOM response
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BomLine {
    #[serde(rename = "designEntry")]
    design_entry: DesignBomEntry,
    #[serde(rename = "offerIds")]
    offer_ids: Vec<String>,
    #[serde(rename = "offerStockClasses")]
    offer_stock_classes: HashMap<String, SourcingStockClass>,
    #[serde(rename = "match")]
    match_status: BomMatchStatus,
    #[serde(rename = "selectedOfferId")]
    selected_offer_id: Option<String>,
}

fn bom_line_no_match(bom_line: &BomLine) -> bool {
    bom_line.match_status == BomMatchStatus::Failed
}

fn retained_selected_offer_id(offers: &[Offer], selected_offer_id: Option<&str>) -> Option<String> {
    selected_offer_id
        .filter(|selected_id| {
            offers
                .iter()
                .any(|offer| offer.id.as_deref() == Some(*selected_id))
        })
        .map(str::to_owned)
}

/// Response from /api/boms/match endpoint
#[derive(Debug, Serialize, Deserialize)]
struct MatchBomResponse {
    results: Vec<BomLine>,
    offers: HashMap<String, ComponentOffer>,
}

#[derive(Debug, Default)]
struct PreparedBomMatch {
    availability: HashMap<String, Availability>,
}

impl PreparedBomMatch {
    fn apply(self, bom: &mut pcb_sch::bom::Bom) {
        for (path, availability) in &self.availability {
            let Some((mpn, manufacturer)) = availability.compatible_part() else {
                continue;
            };
            let entry = bom
                .entries
                .get_mut(path)
                .expect("validated BOM selection path");
            entry.mpn = Some(mpn.to_string());
            entry.manufacturer = Some(manufacturer.to_string());
        }
        bom.availability = self.availability;
    }
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
        (Some(distributor), Some(id)) if distributor.eq_ignore_ascii_case("lcsc") => {
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
        stock_class: SourcingStockClass::Unknown,
        price: offer.unit_price_at_qty(target_qty),
        stock: offer.stock_available.unwrap_or_default(),
        alt_stock,
        price_breaks: offer
            .price_breaks
            .as_ref()
            .map(|pbs| pbs.iter().map(|pb| (pb.qty, pb.price)).collect()),
        lcsc_part_ids,
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

fn bom_match_request(bom_entries: &[serde_json::Value], region: Geography) -> serde_json::Value {
    serde_json::json!({
        "designBom": bom_entries,
        "format": "normalized",
        "boardQuantity": BOARD_QUANTITY,
        "region": region,
    })
}

/// The API plans sourcing for one region per request. US offers rank ahead of Global.
fn call_bom_match_api(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom_entries: &[serde_json::Value],
    timeout_secs: u64,
) -> Result<MatchBomResponse> {
    let url = format!(
        "{}/api/boms/match",
        ctx.api_base_url().trim_end_matches('/')
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;
    let fetch = |region| fetch_regional_bom_match(&client, &url, auth_token, bom_entries, region);
    let (us, global) = std::thread::scope(|scope| {
        let global = scope.spawn(|| fetch(Geography::Global));
        let us = fetch(Geography::Us);
        (us, global.join().expect("BOM match request panicked"))
    });
    merge_regional_bom_matches(us?, global?)
}

fn fetch_regional_bom_match(
    client: &Client,
    url: &str,
    auth_token: Option<&str>,
    bom_entries: &[serde_json::Value],
    region: Geography,
) -> Result<MatchBomResponse> {
    let response = crate::auth::apply_bearer_auth(client.post(url), auth_token)
        .json(&bom_match_request(bom_entries, region))
        .send()
        .context("Failed to send BOM match request")?;
    let status = response.status();
    let response_text = response
        .text()
        .context("Failed to read BOM match response")?;
    anyhow::ensure!(
        status.is_success(),
        "BOM match request failed ({status}): {response_text}"
    );
    serde_json::from_str(&response_text).context("Failed to parse BOM match response")
}

fn merge_regional_bom_matches(
    us: MatchBomResponse,
    global: MatchBomResponse,
) -> Result<MatchBomResponse> {
    anyhow::ensure!(
        us.results.len() == global.results.len(),
        "BOM match regions returned {} and {} results",
        us.results.len(),
        global.results.len()
    );
    let results = us
        .results
        .into_iter()
        .zip(global.results)
        .map(|(mut line, global)| {
            anyhow::ensure!(
                line.design_entry.path == global.design_entry.path,
                "BOM match regions returned lines in different orders"
            );
            line.offer_ids.extend(global.offer_ids);
            line.offer_stock_classes.extend(global.offer_stock_classes);
            line.selected_offer_id = line.selected_offer_id.or(global.selected_offer_id);
            line.match_status = match (line.match_status, global.match_status) {
                (BomMatchStatus::NeedsRetry, _) | (_, BomMatchStatus::NeedsRetry) => {
                    BomMatchStatus::NeedsRetry
                }
                (BomMatchStatus::Failed, status) | (status, _) => status,
            };
            Ok(line)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut offers = us.offers;
    offers.extend(global.offers);
    Ok(MatchBomResponse { results, offers })
}

fn prepare_bom_match(
    bom: &pcb_sch::bom::Bom,
    match_response: &MatchBomResponse,
) -> Result<PreparedBomMatch> {
    let expected_paths = bom.entries.keys().cloned().collect::<HashSet<_>>();
    anyhow::ensure!(
        match_response.results.len() == expected_paths.len(),
        "BOM match response returned {} results for {} requested paths",
        match_response.results.len(),
        expected_paths.len()
    );

    let mut seen_paths = HashSet::with_capacity(expected_paths.len());
    let mut availability = HashMap::with_capacity(expected_paths.len());

    for bom_line in &match_response.results {
        let path = bom_line
            .design_entry
            .path
            .as_deref()
            .context("BOM match response omitted a design entry path")?;
        anyhow::ensure!(
            expected_paths.contains(path),
            "BOM match response returned an unknown path: {path}"
        );
        anyhow::ensure!(
            seen_paths.insert(path.to_string()),
            "BOM match response returned duplicate path: {path}"
        );

        let mut resolved_offers = Vec::with_capacity(bom_line.offer_ids.len());
        for offer_id in &bom_line.offer_ids {
            let offer = match_response.offers.get(offer_id).with_context(|| {
                format!("BOM match response omitted referenced offer {offer_id} for {path}")
            })?;
            anyhow::ensure!(
                offer.id == *offer_id,
                "BOM match response keyed offer {offer_id} with mismatched ID {}",
                offer.id
            );
            anyhow::ensure!(
                bom_line.offer_stock_classes.contains_key(offer_id),
                "BOM match response omitted the stock class for offer {offer_id}"
            );
            resolved_offers.push(offer);
        }

        if let Some(selected_offer_id) = &bom_line.selected_offer_id {
            anyhow::ensure!(
                bom_line.offer_ids.contains(selected_offer_id),
                "BOM match response selected offer {selected_offer_id} outside the ranked offers for {path}"
            );
            let selected_offer = match_response
                .offers
                .get(selected_offer_id)
                .with_context(|| format!("BOM match response omitted offer {selected_offer_id}"))?;

            if bom_line.match_status == BomMatchStatus::Compatible {
                selected_offer
                    .mpn
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .context("Selected compatible offer omitted its MPN")?;
                selected_offer
                    .manufacturer
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .context("Selected compatible offer omitted its manufacturer")?;
            }
        }

        let qty = bom
            .designators
            .iter()
            .filter(|(candidate_path, _)| candidate_path.as_str() == path)
            .count() as i32;
        let target_qty = qty * BOARD_QUANTITY;

        let (us_offers, mut us) =
            summarize_region(&resolved_offers, Geography::Us, target_qty, qty);
        let (global_offers, mut global) =
            summarize_region(&resolved_offers, Geography::Global, target_qty, qty);
        for (summary, offers) in [(&mut us, &us_offers), (&mut global, &global_offers)] {
            if let (Some(summary), Some(offer)) = (summary, offers.first()) {
                summary.stock_class = *bom_line
                    .offer_stock_classes
                    .get(&offer.id)
                    .expect("validated offer stock class");
            }
        }

        let all_offers = us_offers
            .iter()
            .chain(global_offers.iter())
            .map(|offer| offer.to_offer(target_qty))
            .collect::<Vec<_>>();
        let selected_offer_id =
            retained_selected_offer_id(&all_offers, bom_line.selected_offer_id.as_deref());
        availability.insert(
            path.to_string(),
            Availability {
                match_status: Some(bom_line.match_status),
                us,
                global,
                no_match: bom_line_no_match(bom_line),
                selected_offer_id,
                offers: all_offers,
            },
        );
    }

    let mut identical_entries = HashMap::<&BomEntry, (&str, &Availability)>::new();
    for (path, candidate) in &availability {
        let entry = bom.entries.get(path).expect("validated BOM response path");
        if !entry.has_stable_aggregation_identity() {
            continue;
        }
        if let Some((first_path, first)) = identical_entries.get(entry) {
            anyhow::ensure!(
                *first == candidate,
                "BOM match response disagreed for identical entries {first_path} and {path}"
            );
        } else {
            identical_entries.insert(entry, (path, candidate));
        }
    }

    Ok(PreparedBomMatch { availability })
}

fn bom_request_entries(bom: &pcb_sch::bom::Bom) -> Result<Vec<serde_json::Value>> {
    let mut request_bom = bom.clone();
    request_bom.availability.clear();
    serde_json::from_str(&request_bom.ungrouped_json()).context("Failed to parse BOM JSON")
}

/// Match a BOM against live supplier offers and apply the result.
pub fn match_bom_with_context(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom: &mut pcb_sch::bom::Bom,
) -> Result<()> {
    if bom.is_empty() {
        return Ok(());
    }
    let response = call_bom_match_api(
        ctx,
        auth_token,
        &bom_request_entries(bom)?,
        BOM_MATCH_TIMEOUT_SECS,
    )?;
    prepare_bom_match(bom, &response)?.apply(bom);
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

/// Fetch pricing for grouped alternate components as one planned BOM line per group.
pub fn fetch_pricing_grouped_batch(
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
    let match_response = call_bom_match_api(&ctx, auth_token, &bom_entries, 30)?;
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
        *slot = build_search_availability(
            &offers,
            bom_line.selected_offer_id.as_deref(),
            bom_line.match_status,
            bom_line_no_match(bom_line),
        );
    }

    Ok(results)
}

fn build_search_availability(
    offers: &[&ComponentOffer],
    selected_offer_id: Option<&str>,
    match_status: BomMatchStatus,
    no_match: bool,
) -> Availability {
    let (_, us) = summarize_region(offers, Geography::Us, 1, 1);
    let (_, global) = summarize_region(offers, Geography::Global, 1, 1);
    let offers = offers
        .iter()
        .map(|offer| offer.to_offer(1))
        .collect::<Vec<_>>();
    let selected_offer_id = retained_selected_offer_id(&offers, selected_offer_id);

    Availability {
        match_status: Some(match_status),
        us,
        global,
        no_match,
        selected_offer_id,
        offers,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use httpmock::{Method::POST, MockServer};
    use pcb_sch::bom::{Bom, BomEntry, GenericComponent, Resistor};

    use super::*;

    fn bom_line(match_status: BomMatchStatus, offer_ids: Vec<String>) -> BomLine {
        let offer_stock_classes = offer_ids
            .iter()
            .map(|offer_id| (offer_id.clone(), SourcingStockClass::Unknown))
            .collect();
        BomLine {
            design_entry: DesignBomEntry {
                path: Some("root.U1".to_string()),
            },
            offer_ids,
            offer_stock_classes,
            match_status,
            selected_offer_id: None,
        }
    }

    fn test_bom() -> Bom {
        let entry = BomEntry {
            mpn: None,
            alternatives: Vec::new(),
            manufacturer: None,
            package: Some("0603".to_string()),
            value: Some("10kOhm".to_string()),
            description: None,
            generic_data: Some(GenericComponent::Resistor(Resistor {
                resistance: "10kOhm".parse().unwrap(),
                voltage: None,
                power: None,
            })),
            dnp: false,
            skip_bom: false,
            properties: BTreeMap::new(),
        };
        Bom::new(
            HashMap::from([("root.U1".to_string(), entry)]),
            HashMap::from([("root.U1".to_string(), "U1".to_string())]),
        )
    }

    fn compatible_response_for_lines(lines: &[(&str, &str, &str)]) -> serde_json::Value {
        let results = lines
            .iter()
            .copied()
            .map(|(path, offer_id, _)| {
                serde_json::json!({
                    "designEntry": {"path": path},
                    "offerIds": [offer_id],
                    "offerStockClasses": {(offer_id): "PLENTY"},
                    "match": "MATCH_COMPATIBLE",
                    "selectedOfferId": offer_id
                })
            })
            .collect::<Vec<_>>();
        let offers = lines
            .iter()
            .copied()
            .map(|(_, offer_id, mpn)| {
                (
                    offer_id.to_string(),
                    serde_json::json!({
                        "id": offer_id,
                        "geography": "US",
                        "distributor": "testdist",
                        "distributorPartId": "DIST-1",
                        "mpn": mpn,
                        "manufacturer": "API Manufacturer",
                        "priceBreaks": [{"qty": 1, "price": 0.25}],
                        "stockAvailable": 100,
                        "datasheetUrl": format!("https://example.com/{mpn}.pdf"),
                        "partCollections": ["house"]
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({
            "results": results,
            "offers": offers
        })
    }

    fn compatible_response_for(path: &str, offer_id: &str, mpn: &str) -> serde_json::Value {
        compatible_response_for_lines(&[(path, offer_id, mpn)])
    }

    fn compatible_response() -> serde_json::Value {
        compatible_response_for("root.U1", "selected-offer", "API-MPN")
    }

    fn retry_response(path: &str) -> serde_json::Value {
        serde_json::json!({
            "results": [{
                "designEntry": {"path": path},
                "offerIds": [],
                "offerStockClasses": {},
                "match": "MATCH_NEEDS_RETRY",
                "selectedOfferId": null
            }],
            "offers": {}
        })
    }

    fn test_bom_with_second_line() -> Bom {
        let mut bom = test_bom();
        bom.entries
            .insert("root.U2".to_string(), bom.entries["root.U1"].clone());
        bom.designators
            .insert("root.U2".to_string(), "U2".to_string());
        bom
    }

    fn without_offers(mut response: serde_json::Value) -> serde_json::Value {
        for line in response["results"].as_array_mut().unwrap() {
            line["offerIds"] = serde_json::json!([]);
            line["offerStockClasses"] = serde_json::json!({});
            line["selectedOfferId"] = serde_json::Value::Null;
        }
        response["offers"] = serde_json::json!({});
        response
    }

    #[test]
    fn match_status_controls_no_match_detection() {
        assert!(bom_line_no_match(&bom_line(
            BomMatchStatus::Failed,
            vec!["offer-1".to_string()]
        )));

        for status in [
            BomMatchStatus::Exact,
            BomMatchStatus::Compatible,
            BomMatchStatus::Fuzzy,
            BomMatchStatus::NeedsRetry,
        ] {
            assert!(!bom_line_no_match(&bom_line(status, Vec::new())));
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
    fn regional_matches_merge_with_us_ranked_first() {
        let regional = |offer_id: &str, geography: &str| -> MatchBomResponse {
            let mut response = compatible_response_for("root.U1", offer_id, "API-MPN");
            response["offers"][offer_id]["geography"] = serde_json::json!(geography);
            serde_json::from_value(response).unwrap()
        };

        let merged = merge_regional_bom_matches(
            regional("us-offer", "US"),
            regional("lcsc-offer", "GLOBAL"),
        )
        .unwrap();
        let bom = test_bom();
        let prepared = prepare_bom_match(&bom, &merged).unwrap();
        let availability = &prepared.availability["root.U1"];
        assert_eq!(availability.selected_offer_id.as_deref(), Some("us-offer"));
        assert!(availability.us.is_some() && availability.global.is_some());
        assert_eq!(availability.offers.len(), 2);

        let global_only = merge_regional_bom_matches(
            serde_json::from_value(without_offers(compatible_response())).unwrap(),
            regional("lcsc-offer", "GLOBAL"),
        )
        .unwrap();
        assert_eq!(
            global_only.results[0].selected_offer_id.as_deref(),
            Some("lcsc-offer")
        );

        let mut failed = without_offers(compatible_response());
        failed["results"][0]["match"] = serde_json::json!("MATCH_FAILED");
        let failed_us = merge_regional_bom_matches(
            serde_json::from_value(failed).unwrap(),
            regional("lcsc-offer", "GLOBAL"),
        )
        .unwrap();
        assert_eq!(
            failed_us.results[0].match_status,
            BomMatchStatus::Compatible
        );

        let retry = merge_regional_bom_matches(
            regional("us-offer", "US"),
            serde_json::from_value(retry_response("root.U1")).unwrap(),
        )
        .unwrap();
        assert_eq!(retry.results[0].match_status, BomMatchStatus::NeedsRetry);

        let unranked: ComponentOffer = serde_json::from_value(serde_json::json!({
            "id": "uk-offer",
            "geography": "UK"
        }))
        .unwrap();
        assert_eq!(unranked.geography, Geography::Other);
    }

    #[test]
    fn first_ranked_regional_offer_wins() {
        let response: MatchBomResponse =
            serde_json::from_str(include_str!("../tests/fixtures/bom_match_api_order.json"))
                .unwrap();
        let line = &response.results[0];
        let offers: Vec<_> = line
            .offer_ids
            .iter()
            .filter_map(|id| response.offers.get(id))
            .collect();

        let availability = build_search_availability(
            &offers,
            line.selected_offer_id.as_deref(),
            line.match_status,
            false,
        );

        assert_eq!(availability.us.as_ref().unwrap().stock, 5);
        assert_eq!(availability.us.as_ref().unwrap().price, Some(10.0));
        assert_eq!(
            availability.selected_offer_id.as_deref(),
            Some("selected-offer")
        );
        assert_eq!(
            availability.selected_part_collection(),
            Some(PartCollection::Extended)
        );
        assert_eq!(
            availability
                .selected_offer()
                .and_then(|offer| offer.datasheet_url.as_deref()),
            Some("https://example.com/selected.pdf")
        );
        assert_eq!(
            availability.offers[0].part_id.as_deref(),
            Some("SELECTED-OFFER")
        );
        assert_eq!(
            line.offer_stock_classes["selected-offer"],
            SourcingStockClass::Limited
        );
    }

    #[test]
    fn selected_compatible_offer_populates_part_identity() {
        let response: MatchBomResponse = serde_json::from_value(compatible_response()).unwrap();

        let mut bom = test_bom();
        prepare_bom_match(&bom, &response).unwrap().apply(&mut bom);
        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("API-MPN"));
        assert_eq!(
            bom.entries["root.U1"].manufacturer.as_deref(),
            Some("API Manufacturer")
        );
        let selected_offer = bom.availability["root.U1"].selected_offer().unwrap();
        assert_eq!(selected_offer.mpn.as_deref(), Some("API-MPN"));
        assert_eq!(
            selected_offer.manufacturer.as_deref(),
            Some("API Manufacturer")
        );
        assert_eq!(
            selected_offer.datasheet_url.as_deref(),
            Some("https://example.com/API-MPN.pdf")
        );
        assert_eq!(
            bom.availability["root.U1"].match_status,
            Some(BomMatchStatus::Compatible)
        );
        assert_eq!(
            bom.availability["root.U1"].selected_part_collection(),
            Some(PartCollection::House)
        );

        let json: serde_json::Value = serde_json::from_str(&bom.ungrouped_json()).unwrap();
        assert_eq!(json[0]["availability"]["match"], "MATCH_COMPATIBLE");
        assert_eq!(
            json[0]["availability"]["offers"][0]["part_collections"],
            serde_json::json!(["house"])
        );
    }

    #[test]
    fn selected_offer_keeps_its_own_part_datasheet() {
        let response: MatchBomResponse = serde_json::from_value(serde_json::json!({
            "results": [{
                "designEntry": {"path": "root.U1"},
                "offerIds": ["part-a", "part-b"],
                "offerStockClasses": {
                    "part-a": "PLENTY",
                    "part-b": "PLENTY"
                },
                "match": "MATCH_COMPATIBLE",
                "selectedOfferId": "part-b"
            }],
            "offers": {
                "part-a": {
                    "id": "part-a",
                    "geography": "US",
                    "mpn": "PART-A",
                    "manufacturer": "Manufacturer A",
                    "datasheetUrl": "https://example.com/part-a.pdf"
                },
                "part-b": {
                    "id": "part-b",
                    "geography": "US",
                    "mpn": "PART-B",
                    "manufacturer": "Manufacturer B",
                    "datasheetUrl": "https://example.com/part-b.pdf"
                }
            }
        }))
        .unwrap();

        let mut bom = test_bom();
        prepare_bom_match(&bom, &response).unwrap().apply(&mut bom);

        let availability = &bom.availability["root.U1"];
        let selected_offer = availability.selected_offer().unwrap();
        assert_eq!(selected_offer.mpn.as_deref(), Some("PART-B"));
        assert_eq!(
            selected_offer.manufacturer.as_deref(),
            Some("Manufacturer B")
        );
        assert_eq!(
            selected_offer.datasheet_url.as_deref(),
            Some("https://example.com/part-b.pdf")
        );
        assert_eq!(
            availability.offers[0].datasheet_url.as_deref(),
            Some("https://example.com/part-a.pdf")
        );
    }

    #[test]
    fn response_validation_is_atomic() {
        let incomplete: MatchBomResponse = serde_json::from_value(serde_json::json!({
            "results": [],
            "offers": {}
        }))
        .unwrap();
        let bom = test_bom();

        assert!(prepare_bom_match(&bom, &incomplete).is_err());
        assert!(bom.availability.is_empty());
        assert!(bom.entries["root.U1"].mpn.is_none());
    }

    #[test]
    fn identical_entries_require_one_consistent_match() {
        let bom = test_bom_with_second_line();
        let response = serde_json::from_value(compatible_response_for_lines(&[
            ("root.U1", "house-offer", "HOUSE-MPN"),
            ("root.U2", "extended-offer", "EXTENDED-MPN"),
        ]))
        .unwrap();

        let error = prepare_bom_match(&bom, &response).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("BOM match response disagreed for identical entries")
        );
    }

    #[test]
    fn match_requests_each_region_and_skips_empty_boms() {
        let server = MockServer::start();
        let context = WorkspaceContext::from_api_base_url(server.base_url());
        let mut bom = test_bom();
        let entries = bom_request_entries(&bom).unwrap();
        let mocks = [
            (Geography::Us, "us-offer"),
            (Geography::Global, "lcsc-offer"),
        ]
        .map(|(region, offer_id)| {
            let mut response = compatible_response_for("root.U1", offer_id, "API-MPN");
            response["offers"][offer_id]["geography"] = serde_json::json!(region);
            server.mock(|when, then| {
                when.method(POST)
                    .path("/api/boms/match")
                    .json_body(bom_match_request(&entries, region));
                then.status(200).json_body(response);
            })
        });

        let mut empty = Bom::new(HashMap::new(), HashMap::new());
        match_bom_with_context(&context, None, &mut empty).unwrap();
        assert!(empty.availability.is_empty());
        mocks.iter().for_each(|mock| mock.assert_calls(0));

        match_bom_with_context(&context, None, &mut bom).unwrap();
        mocks.iter().for_each(|mock| mock.assert_calls(1));
        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("API-MPN"));
        let availability = &bom.availability["root.U1"];
        assert_eq!(availability.selected_offer_id.as_deref(), Some("us-offer"));
        assert!(availability.us.is_some() && availability.global.is_some());
    }
}
