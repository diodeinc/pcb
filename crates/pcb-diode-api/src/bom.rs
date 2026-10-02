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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ComponentOffer {
    id: String,
    geography: Geography,
    distributor: Option<String>,
    distributor_part_id: Option<String>,
    mpn: Option<String>,
    manufacturer: Option<String>,
    price_breaks: Option<Vec<PriceBreak>>,
    stock_available: Option<i32>,
    product_url: Option<String>,
    datasheet_url: Option<String>,
    #[serde(default)]
    part_collections: Vec<PartCollection>,
}

impl ComponentOffer {
    /// Calculate unit price at a given quantity using price breaks
    fn unit_price_at_qty(&self, qty: i32) -> Option<f64> {
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

    fn lcsc_part_ids(&self) -> Vec<(String, String)> {
        match (self.distributor.as_deref(), &self.distributor_part_id) {
            (Some(distributor), Some(id)) if distributor.eq_ignore_ascii_case("lcsc") => {
                let id = if id.starts_with('C') {
                    id.clone()
                } else {
                    format!("C{id}")
                };
                let url = self
                    .product_url
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

/// One line of the matched BOM response, with its offers ranked best first.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BomLine {
    design_entry: DesignBomEntry,
    offer_ids: Vec<String>,
    offer_stock_classes: HashMap<String, SourcingStockClass>,
    #[serde(rename = "match")]
    match_status: BomMatchStatus,
    selected_offer_id: Option<String>,
}

/// Response from /api/boms/match endpoint
#[derive(Debug, Deserialize)]
struct MatchBomResponse {
    results: Vec<BomLine>,
    offers: HashMap<String, ComponentOffer>,
}

impl MatchBomResponse {
    /// Append another region's offers to each line, ranked after this one's.
    fn merge(mut self, other: Self) -> Self {
        let mut other_lines = other
            .results
            .into_iter()
            .map(|line| (line.design_entry.path.clone(), line))
            .collect::<HashMap<_, _>>();
        for line in &mut self.results {
            let Some(other) = other_lines.remove(&line.design_entry.path) else {
                continue;
            };
            line.offer_ids.extend(other.offer_ids);
            line.offer_stock_classes.extend(other.offer_stock_classes);
            if line.selected_offer_id.is_none() {
                line.selected_offer_id = other.selected_offer_id;
            }
            line.match_status = match (line.match_status, other.match_status) {
                (BomMatchStatus::NeedsRetry, _) | (_, BomMatchStatus::NeedsRetry) => {
                    BomMatchStatus::NeedsRetry
                }
                (BomMatchStatus::Failed, status) | (status, _) => status,
            };
        }
        self.offers.extend(other.offers);
        self
    }

    fn availability(&self, line: &BomLine, target_qty: i32) -> Availability {
        let ranked = line
            .offer_ids
            .iter()
            .filter_map(|id| self.offers.get(id))
            .collect::<Vec<_>>();
        let region = |geography| {
            let regional = ranked
                .iter()
                .copied()
                .filter(|offer| offer.geography == geography)
                .collect::<Vec<_>>();
            let summary = regional.first().map(|best| AvailabilitySummary {
                stock_class: line
                    .offer_stock_classes
                    .get(&best.id)
                    .copied()
                    .unwrap_or_default(),
                price: best.unit_price_at_qty(target_qty),
                stock: best.stock_available.unwrap_or_default(),
                alt_stock: alt_stock(&regional[1..]),
                price_breaks: best
                    .price_breaks
                    .as_ref()
                    .map(|pbs| pbs.iter().map(|pb| (pb.qty, pb.price)).collect()),
                lcsc_part_ids: best.lcsc_part_ids(),
            });
            (regional, summary)
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
            no_match: line.match_status == BomMatchStatus::Failed,
            selected_offer_id: line
                .selected_offer_id
                .clone()
                .filter(|id| offers.iter().any(|offer| offer.id.as_ref() == Some(id))),
            offers,
        }
    }
}

/// Stock of the alternative offers, counting each (distributor, mpn) once at its best price.
fn alt_stock(offers: &[&ComponentOffer]) -> i32 {
    let mut best_by_key: HashMap<(&str, &str), &ComponentOffer> = HashMap::new();
    for offer in offers {
        let key = (
            offer.distributor.as_deref().unwrap_or(""),
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
        .filter_map(|offer| offer.stock_available)
        .sum()
}

/// The API plans sourcing for one region per request. US offers rank ahead of Global.
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
    let fetch = |region: Geography| -> Result<MatchBomResponse> {
        let response = crate::auth::apply_bearer_auth(client.post(&url), auth_token)
            .json(&serde_json::json!({
                "designBom": design_bom,
                "format": "normalized",
                "boardQuantity": BOARD_QUANTITY,
                "region": region,
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
    };
    let (us, global) = std::thread::scope(|scope| {
        let global = scope.spawn(|| fetch(Geography::Global));
        let us = fetch(Geography::Us);
        (us, global.join().expect("BOM match request panicked"))
    });
    Ok(us?.merge(global?))
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
    for line in &response.results {
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
    for line in &response.results {
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

    /// One compatible line for `root.U1` whose offers are ranked as given.
    fn regional_json(geography: Geography, offer_ids: &[&str]) -> serde_json::Value {
        let offers = offer_ids
            .iter()
            .map(|id| {
                let offer = serde_json::json!({
                    "id": id,
                    "geography": geography,
                    "distributor": "testdist",
                    "mpn": format!("{id}-MPN"),
                    "manufacturer": "API Manufacturer",
                    "priceBreaks": [{"qty": 1, "price": 0.25}],
                    "stockAvailable": 100,
                    "datasheetUrl": format!("https://example.com/{id}.pdf"),
                    "partCollections": ["house"]
                });
                (id.to_string(), offer)
            })
            .collect::<serde_json::Map<_, _>>();
        serde_json::json!({
            "results": [{
                "designEntry": {"path": "root.U1"},
                "offerIds": offer_ids,
                "offerStockClasses": offer_ids
                    .iter()
                    .map(|id| (id.to_string(), "PLENTY".into()))
                    .collect::<serde_json::Map<_, _>>(),
                "match": if offer_ids.is_empty() { "MATCH_FAILED" } else { "MATCH_COMPATIBLE" },
                "selectedOfferId": offer_ids.first()
            }],
            "offers": offers
        })
    }

    fn regional(geography: Geography, offer_ids: &[&str]) -> MatchBomResponse {
        serde_json::from_value(regional_json(geography, offer_ids)).unwrap()
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
    fn regions_merge_with_us_ranked_first() {
        let merged = regional(Geography::Us, &["us"]).merge(regional(Geography::Global, &["lcsc"]));
        let mut bom = test_bom();
        apply_bom_match(&mut bom, &merged);
        let availability = &bom.availability["root.U1"];
        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("us-MPN"));
        assert_eq!(availability.selected_offer_id.as_deref(), Some("us"));
        assert_eq!(
            availability.us.as_ref().unwrap().stock_class,
            SourcingStockClass::Plenty
        );
        assert!(availability.global.is_some());
        assert_eq!(availability.offers.len(), 2);

        let us_failed = regional(Geography::Us, &[]).merge(regional(Geography::Global, &["lcsc"]));
        assert_eq!(
            us_failed.results[0].match_status,
            BomMatchStatus::Compatible
        );
        assert_eq!(
            us_failed.results[0].selected_offer_id.as_deref(),
            Some("lcsc")
        );

        let mut retry = regional(Geography::Global, &[]);
        retry.results[0].match_status = BomMatchStatus::NeedsRetry;
        let retry = regional(Geography::Us, &["us"]).merge(retry);
        assert_eq!(retry.results[0].match_status, BomMatchStatus::NeedsRetry);
    }

    #[test]
    fn selected_offer_supplies_the_part_and_first_ranked_the_summary() {
        let mut response = regional(Geography::Us, &["first", "selected"]);
        response.results[0].selected_offer_id = Some("selected".to_string());
        let mut bom = test_bom();
        apply_bom_match(&mut bom, &response);

        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("selected-MPN"));
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
            Some("https://example.com/selected.pdf")
        );
        assert_eq!(availability.us.as_ref().unwrap().alt_stock, 100);
        assert_eq!(availability.offers[0].mpn.as_deref(), Some("first-MPN"));
    }

    #[test]
    fn inconsistent_lines_degrade_without_failing_the_match() {
        let mut response = regional(Geography::Us, &["us"]);
        response.offers.clear();
        response.results.push(BomLine {
            design_entry: DesignBomEntry {
                path: Some("root.UNKNOWN".to_string()),
            },
            ..regional(Geography::Us, &["us"]).results.remove(0)
        });
        let mut bom = test_bom();
        apply_bom_match(&mut bom, &response);

        assert_eq!(bom.availability.len(), 1);
        assert!(bom.entries["root.U1"].mpn.is_none());
        assert_eq!(bom.availability["root.U1"].selected_offer_id, None);

        let unranked: ComponentOffer =
            serde_json::from_value(serde_json::json!({"id": "uk", "geography": "UK"})).unwrap();
        assert_eq!(unranked.geography, Geography::Other);
    }

    #[test]
    fn match_requests_each_region_and_skips_empty_boms() {
        let server = MockServer::start();
        let context = WorkspaceContext::from_api_base_url(server.base_url());
        let mut bom = test_bom();
        let design_bom = serde_json::to_value(bom.ungrouped_entries()).unwrap();
        let mocks = [(Geography::Us, "us"), (Geography::Global, "lcsc")].map(|(region, id)| {
            server.mock(|when, then| {
                when.method(POST)
                    .path("/api/boms/match")
                    .json_body(serde_json::json!({
                        "designBom": design_bom,
                        "format": "normalized",
                        "boardQuantity": BOARD_QUANTITY,
                        "region": region,
                    }));
                then.status(200).json_body(regional_json(region, &[id]));
            })
        });

        let mut empty = Bom::new(HashMap::new(), HashMap::new());
        match_bom_with_context(&context, None, &mut empty).unwrap();
        mocks.iter().for_each(|mock| mock.assert_calls(0));

        match_bom_with_context(&context, None, &mut bom).unwrap();
        mocks.iter().for_each(|mock| mock.assert_calls(1));
        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("us-MPN"));
        let availability = &bom.availability["root.U1"];
        assert!(availability.us.is_some() && availability.global.is_some());
    }
}
