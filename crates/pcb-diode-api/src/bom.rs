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

/// Convert a ComponentOffer to a BomOffer for JSON output
fn component_offer_to_bom_offer(
    offer: &ComponentOffer,
    region: &str,
    target_qty: i32,
) -> pcb_sch::BomOffer {
    pcb_sch::BomOffer {
        region: region.to_string(),
        distributor: offer.distributor.clone(),
        distributor_part_id: offer.distributor_part_id.clone(),
        stock: offer.stock_available.unwrap_or(0),
        unit_price: offer.unit_price_at_qty(target_qty),
        mpn: offer.mpn.clone(),
        manufacturer: offer.manufacturer.clone(),
    }
}

/// Call the BOM match API and return parsed response
fn call_bom_match_api(
    auth_token: &str,
    bom_entries: &[serde_json::Value],
    timeout_secs: u64,
) -> Result<MatchBomResponse> {
    let url = format!("{}/api/boms/match", crate::get_api_base_url());

    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
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

    response
        .json()
        .context("Failed to parse BOM match response")
}

/// Fetch BOM matching results from the API and populate availability data
pub fn fetch_and_populate_availability(auth_token: &str, bom: &mut pcb_sch::Bom) -> Result<()> {
    let bom_json = bom.ungrouped_json();
    let bom_entries: Vec<serde_json::Value> =
        serde_json::from_str(&bom_json).context("Failed to parse BOM JSON")?;

    let match_response = call_bom_match_api(auth_token, &bom_entries, 120)?;

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

        // Filter out offers with unreasonable MOQ: moq > target_qty AND price_at_moq > $100
        let moq_acceptable = |o: &ComponentOffer| {
            let moq = o.moq.unwrap_or(1);
            if moq <= target_qty {
                return true;
            }
            // MOQ exceeds target, check if price at MOQ is reasonable (<= $100)
            let price_at_moq = o.unit_price_at_qty(moq).unwrap_or(f64::MAX) * moq as f64;
            price_at_moq <= 100.0
        };

        let us_offers: Vec<&ComponentOffer> = resolved_offers
            .iter()
            .copied()
            .filter(|o| o.geography == Geography::Us)
            .filter(|o| moq_acceptable(o))
            .collect();
        let global_offers: Vec<&ComponentOffer> = resolved_offers
            .iter()
            .copied()
            .filter(|o| o.geography == Geography::Global)
            .filter(|o| moq_acceptable(o))
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

        // Populate all offers for JSON output
        let all_offers: Vec<pcb_sch::BomOffer> = us_offers
            .iter()
            .map(|o| component_offer_to_bom_offer(o, "us", target_qty))
            .chain(
                global_offers
                    .iter()
                    .map(|o| component_offer_to_bom_offer(o, "global", target_qty)),
            )
            .collect();

        bom.offers.insert(path.to_string(), all_offers);
    }

    Ok(())
}

/// Pricing and availability data for a component
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Availability {
    /// Best US availability summary (price @ stock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us: Option<AvailabilitySummary>,
    /// Best Global availability summary (price @ stock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<AvailabilitySummary>,
    /// All raw offers for detailed display
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub offers: Vec<Offer>,
}

/// Compact availability summary for a region
#[derive(Debug, Clone, serde::Serialize)]
pub struct AvailabilitySummary {
    /// Unit price at qty=1
    pub price: Option<f64>,
    /// Stock available (best offer)
    pub stock: i32,
    /// Combined stock from alternative offers
    pub alt_stock: i32,
}

/// Raw offer data for display
#[derive(Debug, Clone, serde::Serialize)]
pub struct Offer {
    pub region: String,
    pub distributor: String,
    pub stock: i32,
    pub price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
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
    let s = n.to_string();
    let mut result = String::new();
    for (i, c) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(c);
    }
    result.chars().rev().collect()
}

/// Fetch pricing for multiple components in a single batch request
pub fn fetch_pricing_batch(
    auth_token: &str,
    components: &[ComponentKey],
) -> Result<Vec<Availability>> {
    if components.is_empty() {
        return Ok(Vec::new());
    }

    // Create BOM entries for all components
    let bom_entries: Vec<_> = components
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut entry = serde_json::json!({
                "path": format!("component_{}", i),
                "designator": format!("X{}", i),
                "mpn": c.mpn,
            });
            if let Some(ref mfr) = c.manufacturer {
                entry["manufacturer"] = serde_json::json!(mfr);
            }
            entry
        })
        .collect();

    let match_response = call_bom_match_api(auth_token, &bom_entries, 30)?;

    // Build results in order
    let mut results: Vec<Availability> = vec![Availability::default(); components.len()];

    for bom_line in match_response.results {
        let path = match &bom_line.design_entry.path {
            Some(p) => p.as_str(),
            None => continue,
        };

        // Parse index from path "component_N"
        let idx = path
            .strip_prefix("component_")
            .and_then(|s| s.parse::<usize>().ok());
        let Some(idx) = idx else { continue };
        if idx >= components.len() {
            continue;
        }

        let resolved_offers: Vec<&ComponentOffer> = bom_line
            .offer_ids
            .iter()
            .filter_map(|id| match_response.offers.get(id))
            .collect();

        results[idx] = extract_component_pricing(&resolved_offers);
    }

    Ok(results)
}

/// Extract Availability from a list of offers
fn extract_component_pricing(offers: &[&ComponentOffer]) -> Availability {
    let us_offers: Vec<_> = offers
        .iter()
        .copied()
        .filter(|o| o.geography == Geography::Us)
        .collect();
    let global_offers: Vec<_> = offers
        .iter()
        .copied()
        .filter(|o| o.geography == Geography::Global)
        .collect();

    // Find best offers
    let best_us = select_best_offer(us_offers.iter().copied(), 1, false);
    let best_global = select_best_offer(global_offers.iter().copied(), 1, false);

    // Build raw offers list (limit to top 10)
    let raw_offers: Vec<Offer> = offers
        .iter()
        .take(10)
        .map(|o| Offer {
            region: match o.geography {
                Geography::Us => "US".to_string(),
                Geography::Global => "Global".to_string(),
            },
            distributor: o.distributor.clone().unwrap_or_else(|| "—".to_string()),
            stock: o.stock_available.unwrap_or(0),
            price: o.unit_price_at_qty(1),
            part_id: o.distributor_part_id.clone(),
        })
        .collect();

    // Calculate alt stock (sum of stock from non-best offers)
    let us_alt_stock: i32 = us_offers
        .iter()
        .filter(|o| best_us.is_none_or(|best| o.id != best.id))
        .map(|o| o.stock_available.unwrap_or(0))
        .sum();
    let global_alt_stock: i32 = global_offers
        .iter()
        .filter(|o| best_global.is_none_or(|best| o.id != best.id))
        .map(|o| o.stock_available.unwrap_or(0))
        .sum();

    Availability {
        us: best_us.map(|o| AvailabilitySummary {
            price: o.unit_price_at_qty(1),
            stock: o.stock_available.unwrap_or(0),
            alt_stock: us_alt_stock,
        }),
        global: best_global.map(|o| AvailabilitySummary {
            price: o.unit_price_at_qty(1),
            stock: o.stock_available.unwrap_or(0),
            alt_stock: global_alt_stock,
        }),
        offers: raw_offers,
    }
}

/// Fetch availability for registry results that have MPN (up to 10)
pub fn fetch_availability_for_results(
    results: &[crate::RegistryPart],
) -> HashMap<usize, Availability> {
    let Ok(token) = crate::auth::get_valid_token() else {
        return HashMap::new();
    };

    let indexed: Vec<_> = results
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            r.mpn.as_ref().map(|mpn| {
                (
                    i,
                    ComponentKey {
                        mpn: mpn.clone(),
                        manufacturer: r.manufacturer.clone(),
                    },
                )
            })
        })
        .take(10)
        .collect();

    if indexed.is_empty() {
        return HashMap::new();
    }

    let keys: Vec<_> = indexed.iter().map(|(_, k)| k.clone()).collect();
    let pricing = fetch_pricing_batch(&token, &keys).unwrap_or_default();

    indexed
        .into_iter()
        .zip(pricing)
        .filter(|(_, p)| p.us.is_some() || p.global.is_some() || !p.offers.is_empty())
        .map(|((idx, _), p)| (idx, p))
        .collect()
}
