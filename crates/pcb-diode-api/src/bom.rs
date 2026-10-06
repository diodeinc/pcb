use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

use pcb_sch::bom::{
    Availability, AvailabilitySummary, BOARD_QUANTITY, BomMatchStatus, Offer, PartCollection,
    SourcingStockClass,
};

use crate::WorkspaceContext;

const BOM_MATCH_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Money {
    amount_minor: f64,
    currency: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PriceBreak {
    quantity: i32,
    unit_price: Money,
}

/// Geography/region for an offer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComponentOffer {
    id: String,
    geography: Geography,
    seller_name: String,
    seller_sku: Option<String>,
    mpn: Option<String>,
    manufacturer: Option<String>,
    #[serde(default)]
    price_breaks: Vec<PriceBreak>,
    market_stock: Option<i32>,
    seller_url: Option<String>,
    datasheet_url: Option<String>,
    #[serde(default)]
    part_collections: Vec<PartCollection>,
}

impl ComponentOffer {
    /// USD price breaks as (quantity, unit price in dollars).
    fn usd_price_breaks(&self) -> Vec<(i32, f64)> {
        self.price_breaks
            .iter()
            .filter(|pb| pb.unit_price.currency == "USD")
            .map(|pb| (pb.quantity, pb.unit_price.amount_minor / 100.0))
            .collect()
    }

    /// Calculate unit price at a given quantity using price breaks
    fn unit_price_at_qty(&self, qty: i32) -> Option<f64> {
        let breaks = self.usd_price_breaks();
        // Highest break <= qty, or lowest break if none apply
        breaks
            .iter()
            .filter(|(quantity, _)| *quantity <= qty)
            .max_by_key(|(quantity, _)| *quantity)
            .or_else(|| breaks.iter().min_by_key(|(quantity, _)| *quantity))
            .map(|(_, price)| *price)
    }

    fn to_offer(&self, qty: i32) -> Offer {
        Offer {
            id: Some(self.id.clone()),
            region: self.geography.to_string(),
            distributor: self.seller_name.clone(),
            stock: self.market_stock.unwrap_or_default(),
            price: self.unit_price_at_qty(qty),
            part_id: self.seller_sku.clone(),
            mpn: self.mpn.clone(),
            manufacturer: self.manufacturer.clone(),
            datasheet_url: self.datasheet_url.clone(),
            part_collections: self.part_collections.clone(),
        }
    }

    fn lcsc_part_ids(&self) -> Vec<(String, String)> {
        match &self.seller_sku {
            Some(id) if self.seller_name.eq_ignore_ascii_case("lcsc") => {
                let id = if id.starts_with('C') {
                    id.clone()
                } else {
                    format!("C{id}")
                };
                let url = self
                    .seller_url
                    .clone()
                    .unwrap_or_else(|| format!("https://lcsc.com/product-detail/{id}.html"));
                vec![(id, url)]
            }
            _ => vec![],
        }
    }
}

#[derive(Debug, Deserialize)]
struct DesignBomEntry {
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RankedOffer {
    offer_id: String,
    stock_class: SourcingStockClass,
}

/// One line of the matched BOM response, with each region's offers ranked best first.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BomLine {
    design_entry: DesignBomEntry,
    #[serde(rename = "match")]
    match_status: BomMatchStatus,
    ranked: HashMap<Geography, Vec<RankedOffer>>,
}

/// Response from /api/boms/match endpoint
#[derive(Debug, Deserialize)]
struct MatchBomResponse {
    #[serde(alias = "results")]
    lines: Vec<BomLine>,
    offers: HashMap<String, ComponentOffer>,
}

impl MatchBomResponse {
    fn availability(&self, line: &BomLine, target_qty: i32) -> Availability {
        let region = |geography| {
            let ranked = line
                .ranked
                .get(&geography)
                .into_iter()
                .flatten()
                .filter_map(|rank| Some((rank, self.offers.get(&rank.offer_id)?)))
                .collect::<Vec<_>>();
            let summary = ranked.first().map(|(rank, best)| AvailabilitySummary {
                stock_class: rank.stock_class,
                price: best.unit_price_at_qty(target_qty),
                stock: best.market_stock.unwrap_or_default(),
                alt_stock: alt_stock(ranked[1..].iter().map(|(_, offer)| *offer)),
                price_breaks: Some(best.usd_price_breaks()),
                lcsc_part_ids: best.lcsc_part_ids(),
            });
            let offers = ranked
                .into_iter()
                .map(|(_, offer)| offer)
                .collect::<Vec<_>>();
            (offers, summary)
        };
        let (us_offers, us) = region(Geography::Us);
        let (global_offers, global) = region(Geography::Global);
        let offers = us_offers
            .iter()
            .chain(&global_offers)
            .map(|offer| offer.to_offer(target_qty))
            .collect::<Vec<_>>();

        Availability {
            match_status: Some(line.match_status),
            us,
            global,
            selected_offer_id: us_offers
                .first()
                .or(global_offers.first())
                .map(|offer| offer.id.clone()),
            offers,
        }
    }
}

/// Stock of the alternative offers, counting each (seller, mpn) once at its best price.
fn alt_stock<'a>(offers: impl Iterator<Item = &'a ComponentOffer>) -> i32 {
    let mut best_by_key: HashMap<(&str, &str), &ComponentOffer> = HashMap::new();
    for offer in offers {
        let key = (
            offer.seller_name.as_str(),
            offer.mpn.as_deref().unwrap_or(""),
        );
        let price = |offer: &ComponentOffer| offer.unit_price_at_qty(1).unwrap_or(f64::MAX);
        if best_by_key
            .get(&key)
            .is_none_or(|existing| price(offer) < price(existing))
        {
            best_by_key.insert(key, offer);
        }
    }
    best_by_key
        .values()
        .filter_map(|offer| offer.market_stock)
        .sum()
}

/// One request ranks offers for both regions; US offers rank ahead of Global.
fn call_bom_match_api(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    design_bom: &serde_json::Value,
    timeout_secs: u64,
) -> Result<MatchBomResponse> {
    let url = format!(
        "{}/api/boms/match",
        ctx.api_base_url().trim_end_matches('/')
    );
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;
    let response = crate::auth::apply_bearer_auth(client.post(&url), auth_token)
        .json(&serde_json::json!({
            "designBom": design_bom,
            "boardQuantity": BOARD_QUANTITY,
            "regions": [Geography::Us, Geography::Global],
        }))
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

/// Match a BOM against live supplier offers.
pub fn match_bom_with_context(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom: &mut pcb_sch::bom::Bom,
) -> Result<()> {
    if bom.is_empty() {
        return Ok(());
    }
    let design_bom = serde_json::to_value(bom.ungrouped_entries())?;
    let response = call_bom_match_api(ctx, auth_token, &design_bom, BOM_MATCH_TIMEOUT_SECS)?;
    apply_bom_match(bom, &response);
    Ok(())
}

fn apply_bom_match(bom: &mut pcb_sch::bom::Bom, response: &MatchBomResponse) {
    for line in &response.lines {
        let Some(path) = line.design_entry.path.as_deref() else {
            continue;
        };
        let Some(entry) = bom.entries.get_mut(path) else {
            continue;
        };
        let availability = response.availability(line, BOARD_QUANTITY);
        if let Some((mpn, manufacturer)) = availability.compatible_part() {
            entry.mpn = Some(mpn.to_string());
            entry.manufacturer = Some(manufacturer.to_string());
        }
        bom.availability.insert(path.to_string(), availability);
    }
}

/// Component key for pricing requests
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct ComponentKey {
    pub mpn: String,
    pub manufacturer: Option<String>,
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
    !availability.offers.is_empty()
}

/// Fetch pricing for grouped alternate components as one planned BOM line per group.
pub fn fetch_pricing_grouped_batch(
    auth_token: Option<&str>,
    groups: &[Vec<ComponentKey>],
) -> Result<Vec<Availability>> {
    let part = |component: &ComponentKey| {
        let mut part = serde_json::json!({ "mpn": component.mpn });
        if let Some(manufacturer) = &component.manufacturer {
            part["manufacturer"] = serde_json::json!(manufacturer);
        }
        part
    };
    let design_bom = groups
        .iter()
        .enumerate()
        .filter_map(|(index, group)| {
            let (primary, alternatives) = group.split_first()?;
            let mut entry = part(primary);
            entry["path"] = serde_json::json!(format!("component_{index}"));
            entry["designator"] = serde_json::json!(format!("X{index}"));
            entry["alternatives"] = alternatives.iter().map(part).collect();
            Some(entry)
        })
        .collect::<Vec<_>>();
    let mut results = vec![Availability::default(); groups.len()];
    if design_bom.is_empty() {
        return Ok(results);
    }

    let ctx = WorkspaceContext::from_cwd().unwrap_or_default();
    let response = call_bom_match_api(&ctx, auth_token, &design_bom.into(), 30)?;
    for line in &response.lines {
        let slot = line
            .design_entry
            .path
            .as_deref()
            .and_then(|path| path.strip_prefix("component_")?.parse::<usize>().ok())
            .and_then(|index| results.get_mut(index));
        if let Some(slot) = slot {
            *slot = response.availability(line, 1);
        }
    }

    Ok(results)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use httpmock::{Method::POST, MockServer};
    use pcb_sch::bom::{Bom, BomEntry, GenericComponent, Resistor};

    use super::*;

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

    /// One compatible line for `root.U1` with each region's offers ranked as given.
    fn response_json(us: &[&str], global: &[&str]) -> serde_json::Value {
        let ranked = |ids: &[&str]| {
            ids.iter()
                .map(|id| serde_json::json!({"offerId": id, "stockClass": "PLENTY"}))
                .collect::<Vec<_>>()
        };
        let offers = [(Geography::Us, us), (Geography::Global, global)]
            .into_iter()
            .flat_map(|(geography, ids)| ids.iter().map(move |id| (geography, id)))
            .map(|(geography, id)| {
                let offer = serde_json::json!({
                    "id": id,
                    "geography": geography,
                    "sellerName": "testdist",
                    "mpn": format!("{id}-MPN"),
                    "manufacturer": "API Manufacturer",
                    "priceBreaks": [
                        {"quantity": 1, "unitPrice": {"amountMinor": 25, "currency": "USD"}},
                        {"quantity": 1, "unitPrice": {"amountMinor": 1, "currency": "EUR"}}
                    ],
                    "marketStock": 100,
                    "datasheetUrl": format!("https://example.com/{id}.pdf"),
                    "partCollections": ["house"]
                });
                (id.to_string(), offer)
            })
            .collect::<serde_json::Map<_, _>>();
        let matched = !us.is_empty() || !global.is_empty();
        serde_json::json!({
            "lines": [{
                "designEntry": {"path": "root.U1"},
                "match": if matched { "MATCH_COMPATIBLE" } else { "MATCH_FAILED" },
                "ranked": {"US": ranked(us), "GLOBAL": ranked(global)}
            }],
            "offers": offers
        })
    }

    fn response(us: &[&str], global: &[&str]) -> MatchBomResponse {
        serde_json::from_value(response_json(us, global)).unwrap()
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
    fn us_offers_rank_ahead_of_global() {
        let mut bom = test_bom();
        apply_bom_match(&mut bom, &response(&["us"], &["lcsc"]));
        let availability = &bom.availability["root.U1"];
        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("us-MPN"));
        assert_eq!(availability.selected_offer_id.as_deref(), Some("us"));
        assert_eq!(
            availability.us.as_ref().unwrap().stock_class,
            SourcingStockClass::Plenty
        );
        assert!(availability.global.is_some());
        assert_eq!(availability.offers.len(), 2);

        let mut bom = test_bom();
        apply_bom_match(&mut bom, &response(&[], &["lcsc"]));
        assert_eq!(
            bom.availability["root.U1"].selected_offer_id.as_deref(),
            Some("lcsc")
        );
    }

    #[test]
    fn best_ranked_offer_supplies_the_part_and_usd_prices() {
        let mut bom = test_bom();
        apply_bom_match(&mut bom, &response(&["first", "second"], &[]));

        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("first-MPN"));
        let availability = &bom.availability["root.U1"];
        assert_eq!(
            availability.selected_part_collection(),
            Some(PartCollection::House)
        );
        assert_eq!(
            availability
                .selected_offer()
                .unwrap()
                .datasheet_url
                .as_deref(),
            Some("https://example.com/first.pdf")
        );
        let us = availability.us.as_ref().unwrap();
        assert_eq!(us.price, Some(0.25));
        assert_eq!(us.price_breaks, Some(vec![(1, 0.25)]));
        assert_eq!(us.alt_stock, 100);
    }

    #[test]
    fn inconsistent_lines_degrade_without_failing_the_match() {
        let mut matched = response(&["us"], &[]);
        matched.offers.clear();
        let mut unknown = response(&["us"], &[]).lines.remove(0);
        unknown.design_entry.path = Some("root.UNKNOWN".to_string());
        matched.lines.push(unknown);
        let mut bom = test_bom();
        apply_bom_match(&mut bom, &matched);

        assert_eq!(bom.availability.len(), 1);
        assert!(bom.entries["root.U1"].mpn.is_none());
        assert_eq!(bom.availability["root.U1"].selected_offer_id, None);

        let unranked: ComponentOffer = serde_json::from_value(
            serde_json::json!({"id": "uk", "geography": "UK", "sellerName": "x"}),
        )
        .unwrap();
        assert_eq!(unranked.geography, Geography::Other);
    }

    #[test]
    fn match_requests_both_regions_once_and_skips_empty_boms() {
        let server = MockServer::start();
        let context = WorkspaceContext::from_api_base_url(server.base_url());
        let mut bom = test_bom();
        let design_bom = serde_json::to_value(bom.ungrouped_entries()).unwrap();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .json_body(serde_json::json!({
                    "designBom": design_bom,
                    "boardQuantity": BOARD_QUANTITY,
                    "regions": ["US", "GLOBAL"],
                }));
            then.status(200)
                .json_body(response_json(&["us"], &["lcsc"]));
        });

        let mut empty = Bom::new(HashMap::new(), HashMap::new());
        match_bom_with_context(&context, None, &mut empty).unwrap();
        mock.assert_calls(0);

        match_bom_with_context(&context, None, &mut bom).unwrap();
        mock.assert_calls(1);
        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("us-MPN"));
        let availability = &bom.availability["root.U1"];
        assert!(availability.us.is_some() && availability.global.is_some());
    }
}
