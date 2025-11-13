use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

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

            let mut stock_total = 0;
            let mut cheapest_single = None;
            let mut cheapest_20x = None;
            let mut lcsc_part_id = None;
            let mut product_url = None;

            for offer in &result.offers {
                if offer.distributor.as_deref() == Some("lcsc") && lcsc_part_id.is_none() {
                    lcsc_part_id = offer.distributor_part_id.as_ref().map(|id| {
                        // Ensure LCSC part IDs have "C" prefix
                        if id.starts_with('C') {
                            id.clone()
                        } else {
                            format!("C{}", id)
                        }
                    });
                    product_url = offer.product_url.clone();
                }

                stock_total += offer.stock_available.unwrap_or(0);

                if let Some(unit_price) = offer.unit_price_at_qty(qty) {
                    cheapest_single =
                        Some(cheapest_single.map_or(unit_price, |p: f64| p.min(unit_price)));
                }

                if let Some(unit_price) = offer.unit_price_at_qty(qty * 20) {
                    let total_price_20x = unit_price * (qty * 20) as f64;
                    cheapest_20x =
                        Some(cheapest_20x.map_or(total_price_20x, |p: f64| p.min(total_price_20x)));
                }
            }

            bom.availability.insert(
                path.to_string(),
                pcb_sch::AvailabilityData {
                    stock_total,
                    price_single: cheapest_single,
                    price_20x: cheapest_20x,
                    lcsc_part_id,
                    product_url,
                },
            );
        }
    }

    Ok(())
}
