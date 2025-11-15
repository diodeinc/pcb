use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use pcb_sch::bom::availability::{is_small_generic_passive, tier_for_stock, NUM_BOARDS};

/// Price break structure
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PriceBreak {
    pub qty: i32,
    pub price: f64,
}

/// Offer with match key - represents a distributor part availability
#[derive(Debug, Clone, Deserialize)]
pub struct OfferWithMatchKey {
    #[serde(rename = "componentOfferId")]
    pub component_offer_id: String,

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

impl OfferWithMatchKey {
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

/// Match result for a single design entry
#[derive(Debug, Deserialize)]
pub struct DesignMatchResult {
    #[serde(rename = "designIndex")]
    pub design_index: usize,
    pub offers: Vec<OfferWithMatchKey>,
}

/// Response from /api/boms/match endpoint
#[derive(Debug, Deserialize)]
pub struct MatchBomResponse {
    pub results: Vec<DesignMatchResult>,
}

/// Compare offers within the same tier: prefer lower price, then higher stock
#[inline]
fn within_tier_cmp(a: &OfferWithMatchKey, b: &OfferWithMatchKey, qty: i32) -> std::cmp::Ordering {
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
fn select_best_offer(
    offers: &[OfferWithMatchKey],
    qty: i32,
    is_small_passive: bool,
) -> Option<&OfferWithMatchKey> {
    offers.iter().min_by(|a, b| {
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
        .json(&serde_json::json!({ "designBom": bom_entries }))
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
    for result in match_response.results {
        if let Some(entry_json) = bom_entries.get(result.design_index) {
            let path = entry_json["path"].as_str().unwrap();

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

            // Select the best offer based on availability tier (Plenty > Limited > None)
            // then price within tier. This single offer is used for all downstream data.
            let best_offer = select_best_offer(&result.offers, qty, is_small_passive);

            // Extract all data from the best matched offer
            let (stock_total, lcsc_part_ids, price_breaks) = match best_offer {
                Some(offer) => {
                    let stock = offer.stock_available.unwrap_or(0);

                    let lcsc_id = match (
                        offer.distributor.as_deref(),
                        &offer.distributor_part_id,
                        &offer.product_url,
                    ) {
                        (Some("lcsc"), Some(id), Some(url)) => {
                            let formatted_id = if id.starts_with('C') {
                                id.clone()
                            } else {
                                format!("C{}", id)
                            };
                            vec![(formatted_id, url.clone())]
                        }
                        _ => Vec::new(),
                    };

                    // Store price breaks for recalculation with grouped quantities
                    let breaks = offer
                        .price_breaks
                        .as_ref()
                        .map(|pbs| pbs.iter().map(|pb| (pb.qty, pb.price)).collect());

                    (stock, lcsc_id, breaks)
                }
                None => (0, Vec::new(), None),
            };

            bom.availability.insert(
                path.to_string(),
                pcb_sch::AvailabilityData {
                    stock_total,
                    price_breaks,
                    lcsc_part_ids,
                },
            );
        }
    }

    Ok(())
}
