use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Number of boards to use for availability and pricing calculations
const NUM_BOARDS: i32 = 20;

/// Availability tier for offer selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AvailabilityTier {
    NoInventory = 0,
    Limited = 1,
    Plenty = 2,
}

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

/// Check if this is a small generic passive requiring higher stock threshold
fn is_small_generic_passive(entry: &pcb_sch::BomEntry) -> bool {
    let is_generic_passive = matches!(
        entry.generic_data,
        Some(pcb_sch::GenericComponent::Resistor(_) | pcb_sch::GenericComponent::Capacitor(_))
    );
    let is_small_package = matches!(entry.package.as_deref(), Some("0201" | "0402" | "0603"));

    is_generic_passive && is_small_package
}

/// Determine the availability tier for an offer
fn get_availability_tier(stock: i32, qty: i32, is_small_passive: bool) -> AvailabilityTier {
    if stock == 0 {
        AvailabilityTier::NoInventory
    } else {
        let required_stock = if is_small_passive {
            100
        } else {
            qty * NUM_BOARDS
        };
        if stock >= required_stock {
            AvailabilityTier::Plenty
        } else {
            AvailabilityTier::Limited
        }
    }
}

/// Rank availability tier (lower is better)
#[inline]
fn tier_rank(tier: AvailabilityTier) -> u8 {
    match tier {
        AvailabilityTier::Plenty => 0,
        AvailabilityTier::Limited => 1,
        AvailabilityTier::NoInventory => 2,
    }
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
        let tier_a = get_availability_tier(stock_a, qty, is_small_passive);
        let tier_b = get_availability_tier(stock_b, qty, is_small_passive);

        tier_rank(tier_a)
            .cmp(&tier_rank(tier_b))
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
            let is_small_passive = is_small_generic_passive(bom_entry);

            // Select the best offer based on availability tier (Plenty > Limited > None)
            // then price within tier. This single offer is used for all downstream data.
            let best_offer = select_best_offer(&result.offers, qty, is_small_passive);

            // Extract all data from the best matched offer
            let (stock_total, lcsc_part_ids, price_single, price_boards) = match best_offer {
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

                    let single = offer.unit_price_at_qty(qty);
                    let unit_boards = offer.unit_price_at_qty(qty * NUM_BOARDS);
                    let total_boards = unit_boards.map(|p| p * (qty * NUM_BOARDS) as f64);

                    (stock, lcsc_id, single, total_boards)
                }
                None => (0, Vec::new(), None, None),
            };

            bom.availability.insert(
                path.to_string(),
                pcb_sch::AvailabilityData {
                    stock_total,
                    price_single,
                    price_boards,
                    lcsc_part_ids,
                },
            );
        }
    }

    Ok(())
}
