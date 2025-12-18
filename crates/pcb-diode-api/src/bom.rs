use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

use pcb_sch::bom::availability::{is_small_generic_passive, tier_for_stock, NUM_BOARDS};

/// Price break structure
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PriceBreak {
    pub qty: i32,
    pub price: f64,
}

/// Geography/region for an offer
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Geography {
    Us,
    Global,
}

/// Component offer - represents a distributor part availability
#[derive(Debug, Clone, Deserialize)]
pub struct ComponentOffer {
    pub id: String,

    // Geography/region
    pub geography: Geography,

    // Part identification
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor: Option<String>,
    #[serde(rename = "distributorPartId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor_part_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,

    // Availability & pricing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub moq: Option<i32>,
    #[serde(rename = "leadTimeDays")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lead_time_days: Option<i32>,
    #[serde(rename = "priceBreaks")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_breaks: Option<Vec<PriceBreak>>,
    #[serde(rename = "stockAvailable")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stock_available: Option<i32>,

    // Links
    #[serde(rename = "productUrl")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_url: Option<String>,
}

impl ComponentOffer {
    /// Calculate unit price at a given quantity using price breaks
    pub fn unit_price_at_qty(&self, qty: i32) -> Option<f64> {
        let price_breaks = self.price_breaks.as_ref()?;
        if price_breaks.is_empty() {
            return None;
        }

        // Find the highest quantity break that's <= our target quantity
        let mut best_break: Option<&PriceBreak> = None;
        for pb in price_breaks {
            if pb.qty <= qty {
                if let Some(current_best) = best_break {
                    if pb.qty > current_best.qty {
                        best_break = Some(pb);
                    }
                } else {
                    best_break = Some(pb);
                }
            }
        }

        // If no break applies, use the lowest quantity break
        if best_break.is_none() {
            best_break = price_breaks.iter().min_by_key(|pb| pb.qty);
        }

        best_break.map(|pb| pb.price)
    }
}

/// Design BOM entry structure from the API
#[derive(Debug, Deserialize)]
pub struct DesignBomEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub designator: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

/// BOM Line - represents a single line in the matched BOM response
#[derive(Debug, Deserialize)]
pub struct BomLine {
    pub id: String,
    #[serde(rename = "designEntry")]
    pub design_entry: DesignBomEntry,
    #[serde(rename = "offerIds")]
    pub offer_ids: Vec<String>,
    #[serde(rename = "selectedOfferId")]
    pub selected_offer_id: Option<String>,
}

/// Response from /api/boms/match endpoint
#[derive(Debug, Deserialize)]
pub struct MatchBomResponse {
    pub results: Vec<BomLine>,
    pub offers: HashMap<String, ComponentOffer>,
}

/// Compare offers within the same tier: prefer lower price, then higher stock
#[inline]
fn within_tier_cmp(a: &ComponentOffer, b: &ComponentOffer, qty: i32) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;

    let price_a = a.unit_price_at_qty(qty * NUM_BOARDS);
    let price_b = b.unit_price_at_qty(qty * NUM_BOARDS);

    match (price_a, price_b) {
        (Some(pa), Some(pb)) => pa
            .partial_cmp(&pb)
            .unwrap_or(Equal)
            .then_with(|| b.stock_available.cmp(&a.stock_available)),
        (None, None) => b.stock_available.cmp(&a.stock_available),
        (Some(_), None) => Less,
        (None, Some(_)) => Greater,
    }
}

/// Select the best offer: Plenty > Limited > None tier, then lowest price within tier.
/// Single-pass, allocation-free selection using iterator comparator.
fn select_best_offer<'a>(
    offers: impl Iterator<Item = &'a ComponentOffer>,
    qty: i32,
    is_small_passive: bool,
) -> Option<&'a ComponentOffer> {
    offers.min_by(|a, b| {
        let stock_a = a.stock_available.unwrap_or(0);
        let stock_b = b.stock_available.unwrap_or(0);
        let tier_a = tier_for_stock(stock_a, qty, is_small_passive);
        let tier_b = tier_for_stock(stock_b, qty, is_small_passive);

        tier_a
            .rank()
            .cmp(&tier_b.rank())
            .then_with(|| within_tier_cmp(a, b, qty))
    })
}

/// Calculate alt stock from offers, deduplicating by (distributor, mpn).
/// Assumes offers are already filtered by MOQ.
fn calculate_alt_stock(
    offers: &[&ComponentOffer],
    best_offer: Option<&ComponentOffer>,
    target_qty: i32,
) -> i32 {
    use std::collections::HashMap;

    // Exclude best offer
    let alt_offers: Vec<_> = offers
        .iter()
        .filter(|o| best_offer.is_none_or(|best| o.id != best.id))
        .collect();

    // Deduplicate by (distributor, mpn), keeping the one with best price at target_qty
    let mut best_by_dist_mpn: HashMap<(String, String), &ComponentOffer> = HashMap::new();
    for offer in alt_offers {
        let dist = offer.distributor.clone().unwrap_or_default();
        let mpn = offer.mpn.clone().unwrap_or_default();
        let key = (dist, mpn);

        let dominated = best_by_dist_mpn.get(&key).is_some_and(|existing| {
            let existing_price = existing.unit_price_at_qty(target_qty).unwrap_or(f64::MAX);
            let offer_price = offer.unit_price_at_qty(target_qty).unwrap_or(f64::MAX);
            offer_price >= existing_price
        });

        if !dominated {
            best_by_dist_mpn.insert(key, offer);
        }
    }

    // Sum stock from deduplicated offers
    best_by_dist_mpn
        .values()
        .map(|o| o.stock_available.unwrap_or(0))
        .sum()
}

/// Extract RegionAvailability from an offer with alt stock total
fn extract_region_availability(
    offer: &ComponentOffer,
    alt_stock_total: i32,
) -> pcb_sch::RegionAvailability {
    let stock = offer.stock_available.unwrap_or(0);

    let lcsc_id = match (offer.distributor.as_deref(), &offer.distributor_part_id) {
        (Some("lcsc"), Some(id)) => {
            let formatted_id = if id.starts_with('C') {
                id.clone()
            } else {
                format!("C{}", id)
            };
            let url = offer.product_url.clone().unwrap_or_else(|| {
                format!("https://lcsc.com/product-detail/{}.html", formatted_id)
            });
            vec![(formatted_id, url)]
        }
        _ => Vec::new(),
    };

    let breaks = offer
        .price_breaks
        .as_ref()
        .map(|pbs| pbs.iter().map(|pb| (pb.qty, pb.price)).collect());

    let offer_mpn = offer.mpn.clone().filter(|s| !s.is_empty());
    let offer_mfr = offer.manufacturer.clone().filter(|s| !s.is_empty());

    pcb_sch::RegionAvailability {
        stock_total: stock,
        alt_stock_total,
        price_breaks: breaks,
        lcsc_part_ids: lcsc_id,
        mpn: offer_mpn,
        manufacturer: offer_mfr,
    }
}

/// Fetch BOM matching results from the API and populate availability data
pub fn fetch_and_populate_availability(auth_token: &str, bom: &mut pcb_sch::Bom) -> Result<()> {
    let api_base_url = crate::get_api_base_url();
    let url = format!("{}/api/boms/match", api_base_url);

    let bom_json = bom.ungrouped_json();
    let bom_entries: Vec<serde_json::Value> =
        serde_json::from_str(&bom_json).context("Failed to parse BOM JSON")?;

    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()?;

    let response = client
        .post(&url)
        .bearer_auth(auth_token)
        .json(&serde_json::json!({ "designBom": bom_entries, "format": "normalized" }))
        .send()
        .context("Failed to send BOM match request")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().unwrap_or_default();
        anyhow::bail!("BOM match request failed ({}): {}", status, error_text);
    }

    let match_response: MatchBomResponse = response
        .json()
        .context("Failed to parse BOM match response")?;

    // Populate availability data
    for bom_line in match_response.results {
        // Get the path from the design entry
        let path = match &bom_line.design_entry.path {
            Some(p) => p.as_str(),
            None => continue,
        };

        if !bom.entries.contains_key(path) {
            continue;
        }

        let qty = bom
            .designators
            .iter()
            .filter(|(p, _)| p.as_str() == path)
            .count() as i32;

        // Get BOM entry to check if it's a small generic passive
        let bom_entry = bom.entries.get(path).unwrap();
        let is_small_passive = is_small_generic_passive(
            bom_entry.generic_data.as_ref(),
            bom_entry.package.as_deref(),
        );

        // Resolve offer IDs to actual offers from the deduplicated offers map
        let resolved_offers: Vec<&ComponentOffer> = bom_line
            .offer_ids
            .iter()
            .filter_map(|id| match_response.offers.get(id))
            .collect();

        // Target quantity for MOQ filtering
        let target_qty = qty * NUM_BOARDS;

        // Collect offers by geography, filtering out offers with MOQ > target_qty
        let us_offers: Vec<&ComponentOffer> = resolved_offers
            .iter()
            .copied()
            .filter(|o| o.geography == Geography::Us)
            .filter(|o| o.moq.unwrap_or(1) <= target_qty)
            .collect();
        let global_offers: Vec<&ComponentOffer> = resolved_offers
            .iter()
            .copied()
            .filter(|o| o.geography == Geography::Global)
            .filter(|o| o.moq.unwrap_or(1) <= target_qty)
            .collect();

        // Select best offers per geography
        let best_us_offer = select_best_offer(us_offers.iter().copied(), qty, is_small_passive);
        let best_global_offer =
            select_best_offer(global_offers.iter().copied(), qty, is_small_passive);

        // Calculate alt stock totals (deduplicated by distributor+mpn, best price wins)
        let us_alt_stock = calculate_alt_stock(&us_offers, best_us_offer, target_qty);
        let global_alt_stock = calculate_alt_stock(&global_offers, best_global_offer, target_qty);

        // Extract RegionAvailability for each geography
        let us_availability = best_us_offer.map(|o| extract_region_availability(o, us_alt_stock));
        let global_availability =
            best_global_offer.map(|o| extract_region_availability(o, global_alt_stock));

        bom.availability.insert(
            path.to_string(),
            pcb_sch::AvailabilityData {
                us: us_availability,
                global: global_availability,
            },
        );
    }

    Ok(())
}
