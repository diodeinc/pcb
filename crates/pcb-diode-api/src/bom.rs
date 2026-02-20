use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use std::{collections::HashMap, time::Duration};

use pcb_sch::bom::availability::{NUM_BOARDS, is_small_generic_passive, tier_for_stock};
use pcb_sch::bom::{Availability, AvailabilitySummary, Offer};

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
    Global,
}

impl std::fmt::Display for Geography {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Us => "US",
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
    moq: Option<i32>,
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

/// BOM Line - represents a single line in the matched BOM response
#[derive(Debug, Deserialize)]
struct BomLine {
    #[serde(rename = "designEntry")]
    design_entry: DesignBomEntry,
    #[serde(rename = "offerIds")]
    offer_ids: Vec<String>,
}

/// Response from /api/boms/match endpoint
#[derive(Debug, Deserialize)]
struct MatchBomResponse {
    results: Vec<BomLine>,
    offers: HashMap<String, ComponentOffer>,
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
    include_internal_fields: bool,
) -> AvailabilitySummary {
    if !include_internal_fields {
        return AvailabilitySummary {
            price: offer.unit_price_at_qty(target_qty),
            stock: offer.stock_available.unwrap_or_default(),
            alt_stock,
            ..Default::default()
        };
    }

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
pub fn fetch_and_populate_availability(
    auth_token: &str,
    bom: &mut pcb_sch::bom::Bom,
) -> Result<()> {
    let bom_json = bom.ungrouped_json();
    let bom_entries: Vec<serde_json::Value> =
        serde_json::from_str(&bom_json).context("Failed to parse BOM JSON")?;

    let match_response = call_bom_match_api(auth_token, &bom_entries, 120)?;

    for bom_line in match_response.results {
        let Some(path) = bom_line.design_entry.path.as_deref() else {
            continue;
        };
        let Some(bom_entry) = bom.entries.get(path) else {
            continue;
        };

        let qty = bom
            .designators
            .iter()
            .filter(|(p, _)| p.as_str() == path)
            .count() as i32;
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

        let target_qty = qty * NUM_BOARDS;

        // Filter: MOQ <= target OR price_at_moq <= $100
        let moq_ok = |o: &&ComponentOffer| {
            let moq = o.moq.unwrap_or(1);
            moq <= target_qty || o.unit_price_at_qty(moq).unwrap_or(f64::MAX) * moq as f64 <= 100.0
        };

        // Process each geography
        let process_geo = |geo: Geography| {
            let offers: Vec<_> = resolved_offers
                .iter()
                .copied()
                .filter(|o| o.geography == geo)
                .filter(moq_ok)
                .collect();
            let best = select_best_offer(offers.iter().copied(), qty, is_small_passive);
            let alt = calculate_alt_stock(&offers, best, qty);
            (offers, best, alt)
        };

        let (us_offers, best_us, us_alt) = process_geo(Geography::Us);
        let (global_offers, best_global, global_alt) = process_geo(Geography::Global);

        // Build offers for JSON output
        let all_offers: Vec<_> = us_offers
            .iter()
            .chain(global_offers.iter())
            .map(|o| o.to_offer(target_qty))
            .collect();

        bom.availability.insert(
            path.to_string(),
            Availability {
                us: best_us.map(|o| build_availability_summary(o, us_alt, target_qty, true)),
                global: best_global
                    .map(|o| build_availability_summary(o, global_alt, target_qty, true)),
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

        let summary_for = |geo: Geography| {
            let filtered: Vec<_> = offers
                .iter()
                .copied()
                .filter(|o| o.geography == geo)
                .collect();
            let best = select_best_offer(filtered.iter().copied(), 1, false);
            let alt = calculate_alt_stock(&filtered, best, 1);
            best.map(|o| build_availability_summary(o, alt, 1, false))
        };

        *slot = Availability {
            us: summary_for(Geography::Us),
            global: summary_for(Geography::Global),
            offers: offers.iter().map(|o| o.to_offer(1)).collect(),
        };
    }

    Ok(results)
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
            Some((
                i,
                ComponentKey {
                    mpn: r.mpn.clone()?,
                    manufacturer: r.manufacturer.clone(),
                },
            ))
        })
        .collect();

    let keys: Vec<_> = indexed.iter().map(|(_, k)| k.clone()).collect();
    let pricing = fetch_pricing_batch(&token, &keys).unwrap_or_default();

    indexed
        .into_iter()
        .zip(pricing)
        .filter(|(_, p)| p.us.is_some() || p.global.is_some() || !p.offers.is_empty())
        .map(|((idx, _), p)| (idx, p))
        .collect()
}
