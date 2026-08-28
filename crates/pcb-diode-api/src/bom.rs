use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

use pcb_sch::{
    AttributeValue, InstanceKind, Schematic,
    bom::{
        Availability, AvailabilitySummary, BOARD_QUANTITY, BomEntry, BomMatchStatus, Offer,
        PartCollection, SourcingStockClass,
    },
};
use pcb_zen_core::{attrs, lang::part::PartValue};

use crate::{
    WorkspaceContext,
    cache::{WriteThroughCache, cache_key, unix_now},
};

const BOM_MATCH_TIMEOUT_SECS: u64 = 120;
const BOM_MATCH_CACHE_NAMESPACE: &str = "bom-match-v2";
const DEFAULT_BOM_CACHE_TTL: Duration = Duration::from_secs(10 * 60);
const SCHEMATIC_BOM_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomMatchMode {
    Online,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BomMatchOptions {
    pub mode: BomMatchMode,
    /// Age at which an online read refreshes a cache entry. Stale entries remain usable.
    pub cache_ttl: Duration,
}

impl Default for BomMatchOptions {
    fn default() -> Self {
        Self {
            mode: BomMatchMode::Online,
            cache_ttl: DEFAULT_BOM_CACHE_TTL,
        }
    }
}

impl BomMatchOptions {
    pub const fn for_schematic(mode: BomMatchMode) -> Self {
        Self {
            mode,
            cache_ttl: SCHEMATIC_BOM_CACHE_TTL,
        }
    }
}

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

struct PreparedBomResponse {
    prepared: PreparedBomMatch,
    retry_paths: HashSet<String>,
}

impl PreparedBomResponse {
    fn into_complete(self) -> Result<PreparedBomMatch> {
        anyhow::ensure!(
            self.retry_paths.is_empty(),
            "BOM matching could not complete"
        );
        Ok(self.prepared)
    }
}

impl PreparedBomMatch {
    fn extend(&mut self, other: Self) {
        self.availability.extend(other.availability);
    }

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

    fn apply_to_schematic(&self, schematic: &mut Schematic) -> usize {
        let mut hydrated = 0;

        for (instance_ref, instance) in &mut schematic.instances {
            if instance.kind != InstanceKind::Component {
                continue;
            }

            let path = instance_ref.instance_path.join(".");
            let Some(availability) = self.availability.get(&path) else {
                continue;
            };

            let has_authored_identity = [
                attrs::PART,
                attrs::MPN,
                "Mpn",
                attrs::MANUFACTURER,
                "Manufacturer",
            ]
            .iter()
            .any(|key| instance.attributes.contains_key(*key));
            let has_authored_datasheet = [attrs::DATASHEET, "Datasheet"]
                .iter()
                .any(|key| instance.attributes.contains_key(*key))
                || instance
                    .attributes
                    .get(attrs::PART)
                    .and_then(|value| match value {
                        AttributeValue::Json(value) => Some(value.clone()),
                        AttributeValue::String(value) => serde_json::from_str(value).ok(),
                        _ => None,
                    })
                    .and_then(|value| value.get("datasheet").cloned())
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .is_some_and(|value| !value.trim().is_empty());

            let projected_datasheet = availability
                .selected_datasheet_url()
                .filter(|_| !has_authored_datasheet);
            let mut changed = false;

            if let Some((mpn, manufacturer)) = availability.compatible_part()
                && !has_authored_identity
            {
                let part = PartValue::new(
                    mpn.to_string(),
                    manufacturer.to_string(),
                    Vec::new(),
                    projected_datasheet.map(str::to_string),
                );

                instance
                    .attributes
                    .insert(attrs::MPN.into(), AttributeValue::String(mpn.to_string()));
                instance.attributes.insert(
                    attrs::MANUFACTURER.into(),
                    AttributeValue::String(manufacturer.to_string()),
                );
                instance.attributes.insert(
                    attrs::PART.into(),
                    AttributeValue::Json(part.to_json_value()),
                );
                changed = true;
            }

            if let Some(datasheet) = projected_datasheet {
                instance.attributes.insert(
                    attrs::DATASHEET.into(),
                    AttributeValue::String(datasheet.to_string()),
                );
                changed = true;
            }

            hydrated += usize::from(changed);
        }

        hydrated
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

fn bom_match_request(bom_entries: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({
        "designBom": bom_entries,
        "format": "normalized",
        "boardQuantity": BOARD_QUANTITY,
        "regions": ["US", "GLOBAL"],
    })
}

fn call_bom_match_api(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom_entries: &[serde_json::Value],
    timeout_secs: u64,
    strict: bool,
) -> Result<MatchBomResponse> {
    let url = bom_match_url(ctx.api_base_url(), strict);
    let request_body = bom_match_request(bom_entries);
    let client = Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()?;
    let response = crate::auth::apply_bearer_auth(client.post(url), auth_token)
        .json(&request_body)
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

fn bom_match_url(api_base_url: &str, strict: bool) -> String {
    let suffix = if strict { "?strict=true" } else { "" };
    format!(
        "{}/api/boms/match{suffix}",
        api_base_url.trim_end_matches('/')
    )
}

fn prepare_bom_match_for_paths(
    bom: &pcb_sch::bom::Bom,
    match_response: &MatchBomResponse,
    expected_paths: &HashSet<String>,
) -> Result<PreparedBomResponse> {
    anyhow::ensure!(
        match_response.results.len() == expected_paths.len(),
        "BOM match response returned {} results for {} requested paths",
        match_response.results.len(),
        expected_paths.len()
    );

    let mut seen_paths = HashSet::with_capacity(expected_paths.len());
    let mut availability = HashMap::with_capacity(expected_paths.len());
    let mut retry_paths = HashSet::new();

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
        if bom_line.match_status == BomMatchStatus::NeedsRetry {
            retry_paths.insert(path.to_string());
            continue;
        }

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

    Ok(PreparedBomResponse {
        prepared: PreparedBomMatch { availability },
        retry_paths,
    })
}

fn prepare_bom_match(
    bom: &pcb_sch::bom::Bom,
    match_response: &MatchBomResponse,
) -> Result<PreparedBomMatch> {
    let expected_paths = bom.entries.keys().cloned().collect();
    prepare_bom_match_for_paths(bom, match_response, &expected_paths)?.into_complete()
}

struct CachedBomGroup {
    prepared: PreparedBomMatch,
    fresh: bool,
}

fn load_cached_bom_group(
    cache: Option<&WriteThroughCache>,
    key: &str,
    bom: &pcb_sch::bom::Bom,
    paths: &[String],
    ttl: Duration,
    now: i64,
) -> Result<Option<CachedBomGroup>> {
    let Some(cache) = cache else {
        return Ok(None);
    };
    let Some(cached) = cache.load(key)? else {
        return Ok(None);
    };
    let mut response: MatchBomResponse = serde_json::from_slice(&cached.value)?;
    for (line, path) in response.results.iter_mut().zip(paths) {
        line.design_entry.path = Some(path.clone());
    }
    let expected_paths = paths.iter().cloned().collect();
    let prepared = prepare_bom_match_for_paths(bom, &response, &expected_paths)?.into_complete()?;
    Ok(Some(CachedBomGroup {
        prepared,
        fresh: cached.is_fresh(ttl, now),
    }))
}

fn bom_request_entries(bom: &pcb_sch::bom::Bom) -> Result<Vec<serde_json::Value>> {
    let mut request_bom = bom.clone();
    request_bom.availability.clear();
    serde_json::from_str(&request_bom.ungrouped_json()).context("Failed to parse BOM JSON")
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum BomMatchGroupKey {
    Part {
        manufacturer: String,
        mpn: String,
        dnp: bool,
        skip_bom: bool,
    },
    Entry(Box<BomEntry>),
    Path(String),
}

fn normalized_part(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_uppercase)
}

/// A conservative pre-match version of the API planner's grouping key.
/// Lines the API may plan together always share this key. The API may split a
/// group further when its discovered offer sets differ.
fn bom_match_group_key(path: &str, entry: &BomEntry) -> BomMatchGroupKey {
    match (
        normalized_part(entry.manufacturer.as_deref()),
        normalized_part(entry.mpn.as_deref()),
    ) {
        (Some(manufacturer), Some(mpn)) => BomMatchGroupKey::Part {
            manufacturer,
            mpn,
            dnp: entry.dnp,
            skip_bom: entry.skip_bom,
        },
        _ if entry.has_stable_aggregation_identity() => {
            BomMatchGroupKey::Entry(Box::new(entry.clone()))
        }
        _ => BomMatchGroupKey::Path(path.to_string()),
    }
}

/// Remove instance-only correlation fields from stable cache identities.
fn bom_match_cache_entry(
    request_entry: serde_json::Value,
    entry: &BomEntry,
) -> Result<serde_json::Value> {
    Ok(if entry.has_stable_aggregation_identity() {
        serde_json::to_value(entry)?
    } else {
        request_entry
    })
}

struct BomMatchRequestGroup {
    paths: Vec<String>,
    entries: Vec<serde_json::Value>,
    cache_key: String,
    cached: Option<CachedBomGroup>,
}

fn bom_match_request_groups(
    url: &str,
    bom: &pcb_sch::bom::Bom,
) -> Result<Vec<BomMatchRequestGroup>> {
    let mut groups = Vec::<(Vec<String>, Vec<serde_json::Value>)>::new();
    let mut group_indices = HashMap::<BomMatchGroupKey, usize>::new();

    for entry in bom_request_entries(bom)? {
        let path = entry
            .get("path")
            .and_then(serde_json::Value::as_str)
            .context("BOM entry omitted its path")?
            .to_string();
        let key = bom_match_group_key(
            &path,
            bom.entries
                .get(&path)
                .context("BOM entry path was not present in the BOM")?,
        );
        let group_index = match group_indices.get(&key) {
            Some(index) => *index,
            None => {
                let index = groups.len();
                group_indices.insert(key, index);
                groups.push((Vec::new(), Vec::new()));
                index
            }
        };
        groups[group_index].0.push(path);
        groups[group_index].1.push(entry);
    }

    groups
        .into_iter()
        .map(|(paths, entries)| {
            let mut lines = paths
                .into_iter()
                .zip(entries)
                .map(|(path, entry)| {
                    let cache_entry = bom_match_cache_entry(
                        entry.clone(),
                        bom.entries.get(&path).expect("validated BOM request path"),
                    )?;
                    let sort_key = serde_json::to_vec(&cache_entry)?;
                    Ok((sort_key, path, entry, cache_entry))
                })
                .collect::<Result<Vec<_>>>()?;
            lines.sort_by(|left, right| left.0.cmp(&right.0));

            let canonical_entries = lines
                .iter()
                .map(|(_, _, _, entry)| entry.clone())
                .collect::<Vec<_>>();
            let request = bom_match_request(&canonical_entries);
            Ok(BomMatchRequestGroup {
                paths: lines.iter().map(|(_, path, _, _)| path.clone()).collect(),
                entries: lines.into_iter().map(|(_, _, entry, _)| entry).collect(),
                cache_key: cache_key(&(url, request))?,
                cached: None,
            })
        })
        .collect()
}

fn serialize_completed_bom_match_group(
    response: &MatchBomResponse,
    paths: &[String],
) -> Result<Vec<u8>> {
    let results = paths
        .iter()
        .map(|path| {
            let mut line = response
                .results
                .iter()
                .find(|line| line.design_entry.path.as_ref() == Some(path))
                .expect("validated BOM match response path")
                .clone();
            line.design_entry.path = None;
            line
        })
        .collect::<Vec<_>>();

    let mut offers = HashMap::new();
    for offer_id in results.iter().flat_map(|line| &line.offer_ids) {
        offers.insert(
            offer_id.clone(),
            response
                .offers
                .get(offer_id)
                .expect("validated BOM match response offer")
                .clone(),
        );
    }
    serde_json::to_vec(&MatchBomResponse { results, offers }).map_err(Into::into)
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
    let response = call_bom_match_api(
        ctx,
        auth_token,
        &bom_request_entries(bom)?,
        BOM_MATCH_TIMEOUT_SECS,
        strict,
    )?;
    prepare_bom_match(bom, &response)?.apply(bom);
    Ok(())
}

pub fn match_bom_with_context(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom: &mut pcb_sch::bom::Bom,
    strict: bool,
    options: BomMatchOptions,
) -> Result<()> {
    let mut cache = open_bom_cache();
    match_bom_with_cache(
        ctx,
        auth_token,
        bom,
        strict,
        options,
        cache.as_mut(),
        unix_now()?,
    )
}

fn open_bom_cache() -> Option<WriteThroughCache> {
    match WriteThroughCache::open(BOM_MATCH_CACHE_NAMESPACE) {
        Ok(cache) => Some(cache),
        Err(error) => {
            log::warn!("Failed to open local BOM cache: {error:#}");
            None
        }
    }
}

fn match_bom_with_cache(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom: &mut pcb_sch::bom::Bom,
    strict: bool,
    options: BomMatchOptions,
    cache: Option<&mut WriteThroughCache>,
    now: i64,
) -> Result<()> {
    prepare_bom_match_with_cache(ctx, auth_token, bom, strict, options, cache, now)?.apply(bom);
    Ok(())
}

fn prepare_bom_match_with_cache(
    ctx: &WorkspaceContext,
    auth_token: Option<&str>,
    bom: &pcb_sch::bom::Bom,
    strict: bool,
    options: BomMatchOptions,
    cache: Option<&mut WriteThroughCache>,
    now: i64,
) -> Result<PreparedBomMatch> {
    let url = bom_match_url(ctx.api_base_url(), strict);
    let mut groups = bom_match_request_groups(&url, bom)?;
    for group in &mut groups {
        group.cached = match load_cached_bom_group(
            cache.as_deref(),
            &group.cache_key,
            bom,
            &group.paths,
            options.cache_ttl,
            now,
        ) {
            Ok(cached) => cached,
            Err(error) => {
                log::warn!("Ignoring invalid BOM cache entry: {error:#}");
                None
            }
        };
    }

    let mut prepared = PreparedBomMatch::default();
    if options.mode == BomMatchMode::Offline {
        for group in groups {
            if let Some(cached) = group.cached {
                prepared.extend(cached.prepared);
            }
        }
        return Ok(prepared);
    }

    let mut refresh_groups = Vec::new();
    for mut group in groups {
        match group.cached.take() {
            Some(cached) if cached.fresh => prepared.extend(cached.prepared),
            cached => {
                group.cached = cached;
                refresh_groups.push(group);
            }
        }
    }

    if refresh_groups.is_empty() {
        return Ok(prepared);
    }

    let refresh_entries = refresh_groups
        .iter()
        .flat_map(|group| group.entries.iter().cloned())
        .collect::<Vec<_>>();
    let expected_paths = refresh_groups
        .iter()
        .flat_map(|group| group.paths.iter().cloned())
        .collect::<HashSet<_>>();
    let live = call_bom_match_api(
        ctx,
        auth_token,
        &refresh_entries,
        BOM_MATCH_TIMEOUT_SECS,
        strict,
    )
    .and_then(|response| {
        let mut prepared = prepare_bom_match_for_paths(bom, &response, &expected_paths)?;
        let retry_paths = prepared.retry_paths.clone();
        let unresolved_paths = refresh_groups
            .iter()
            .filter(|group| group.paths.iter().any(|path| retry_paths.contains(path)))
            .flat_map(|group| group.paths.iter().cloned())
            .collect::<HashSet<_>>();
        for path in &unresolved_paths {
            prepared.prepared.availability.remove(path);
        }
        prepared.retry_paths = unresolved_paths;

        let writes = refresh_groups
            .iter()
            .filter(|group| {
                !group
                    .paths
                    .iter()
                    .any(|path| prepared.retry_paths.contains(path))
            })
            .map(|group| {
                Ok((
                    group.cache_key.clone(),
                    serialize_completed_bom_match_group(&response, &group.paths)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(cache) = cache
            && let Err(error) = cache.store_many(&writes)
        {
            log::warn!("Failed to update local BOM cache: {error:#}");
        }
        Ok(prepared)
    });

    let (live_prepared, unresolved_paths, live_error) = match live {
        Ok(response) => {
            let error = (!response.retry_paths.is_empty())
                .then(|| anyhow::anyhow!("BOM matching could not complete"));
            (response.prepared, response.retry_paths, error)
        }
        Err(error) => (
            PreparedBomMatch::default(),
            refresh_groups
                .iter()
                .flat_map(|group| group.paths.iter().cloned())
                .collect(),
            Some(error),
        ),
    };

    prepared.extend(live_prepared);
    for group in refresh_groups {
        if group
            .paths
            .iter()
            .any(|path| unresolved_paths.contains(path))
            && let Some(cached) = group.cached
        {
            prepared.extend(cached.prepared);
        }
    }
    if let Some(error) = live_error {
        if prepared.availability.is_empty() {
            return Err(error);
        }
        log::warn!("BOM matching incomplete; using available lines: {error:#}");
    }
    Ok(prepared)
}

/// Opportunistically hydrate a schematic from BOM matches.
///
/// Online mode refreshes stale and missing cache groups. Any failure falls back
/// to stale group data, while unresolved components remain unchanged.
pub fn hydrate_schematic_from_bom(
    source_path: &Path,
    schematic: &mut Schematic,
    mode: BomMatchMode,
) {
    if let Err(error) = try_hydrate_schematic_from_bom(source_path, schematic, mode) {
        log::warn!("Ignoring BOM hydration failure: {error:#}");
    }
}

fn try_hydrate_schematic_from_bom(
    source_path: &Path,
    schematic: &mut Schematic,
    mode: BomMatchMode,
) -> Result<usize> {
    let ctx = WorkspaceContext::from_path(source_path);
    let strict = ctx.bom_strict()?;
    let mut cache = open_bom_cache();
    hydrate_schematic_from_bom_with_cache(
        &ctx,
        schematic,
        strict,
        mode,
        cache.as_mut(),
        unix_now()?,
    )
}

fn hydrate_schematic_from_bom_with_cache(
    ctx: &WorkspaceContext,
    schematic: &mut Schematic,
    strict: bool,
    mode: BomMatchMode,
    cache: Option<&mut WriteThroughCache>,
    now: i64,
) -> Result<usize> {
    let mut component_paths = HashSet::new();
    for (instance_ref, instance) in &schematic.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        let path = instance_ref.instance_path.join(".");
        if path.is_empty()
            || instance.reference_designator.is_none()
            || !component_paths.insert(path)
        {
            return Ok(0);
        }
    }

    let bom = schematic.bom().filter_excluded();
    let prepared = prepare_bom_match_with_cache(
        ctx,
        None,
        &bom,
        strict,
        BomMatchOptions::for_schematic(mode),
        cache,
        now,
    )?;
    Ok(prepared.apply_to_schematic(schematic))
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
        *slot = build_search_availability(
            &offers,
            bom_line.selected_offer_id.as_deref(),
            bom_line.match_status,
            bom_line_no_match(bom_line),
        );
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

        *slot = build_search_availability(
            &offers,
            bom_line.selected_offer_id.as_deref(),
            bom_line.match_status,
            bom_line_no_match(&bom_line),
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
        .filter(|offer| offer.geography != Geography::Uk)
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
    use pcb_sch::{AttributeValue, Instance, InstanceRef, ModuleRef, Schematic};

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
            datasheet_url: Some(format!("https://example.com/{id}.pdf")),
            part_collections: Vec::new(),
        }
    }

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

    fn test_bom_at(path: &str, designator: &str) -> Bom {
        let entry = test_bom().entries.remove("root.U1").unwrap();
        Bom::new(
            HashMap::from([(path.to_string(), entry)]),
            HashMap::from([(path.to_string(), designator.to_string())]),
        )
    }

    fn test_schematic() -> Schematic {
        let module = ModuleRef::new("/tmp/root.zen", "Root");
        let instance_ref = InstanceRef::new(module.clone(), vec!["root".into(), "U1".into()]);
        let mut instance = Instance::component(module);
        instance.reference_designator = Some("U1".to_string());
        instance
            .attributes
            .insert("package".into(), AttributeValue::String("0603".into()));
        instance
            .attributes
            .insert("value".into(), AttributeValue::String("10kOhm".into()));
        instance
            .attributes
            .insert("type".into(), AttributeValue::String("resistor".into()));
        instance
            .attributes
            .insert("resistance".into(), AttributeValue::String("10kOhm".into()));

        let mut schematic = Schematic::default();
        schematic.instances.insert(instance_ref, instance);
        schematic
    }

    fn test_component(schematic: &Schematic) -> &Instance {
        schematic
            .instances
            .values()
            .find(|instance| instance.kind == InstanceKind::Component)
            .unwrap()
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

    fn test_bom_with_distinct_second_line() -> Bom {
        let mut bom = test_bom_with_second_line();
        let second = bom.entries.get_mut("root.U2").unwrap();
        second.value = Some("20kOhm".to_string());
        second.generic_data = Some(GenericComponent::Resistor(Resistor {
            resistance: "20kOhm".parse().unwrap(),
            voltage: None,
            power: None,
        }));
        bom
    }

    fn test_identityless_bom() -> Bom {
        let entry = BomEntry {
            mpn: None,
            alternatives: Vec::new(),
            manufacturer: None,
            package: None,
            value: None,
            description: None,
            generic_data: None,
            dnp: false,
            skip_bom: false,
            properties: BTreeMap::from([("empty".to_string(), " ".to_string())]),
        };
        Bom::new(
            HashMap::from([
                ("root.U1".to_string(), entry.clone()),
                ("root.U2".to_string(), entry),
            ]),
            HashMap::from([
                ("root.U1".to_string(), "U1".to_string()),
                ("root.U2".to_string(), "U2".to_string()),
            ]),
        )
    }

    fn cache_for(tempdir: &tempfile::TempDir) -> WriteThroughCache {
        WriteThroughCache::open_at(
            tempdir.path().join("cache.sqlite"),
            BOM_MATCH_CACHE_NAMESPACE,
        )
        .unwrap()
    }

    fn match_options(mode: BomMatchMode) -> BomMatchOptions {
        BomMatchOptions {
            mode,
            ..Default::default()
        }
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

        let availability =
            build_search_availability(&[&uk_offer], None, BomMatchStatus::Exact, false);

        assert!(availability.us.is_none());
        assert!(availability.global.is_none());
        assert!(availability.offers.is_empty());
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
    fn bom_match_url_selects_strict_matching_when_enabled() {
        assert_eq!(
            bom_match_url("https://api.diode.computer", false),
            "https://api.diode.computer/api/boms/match"
        );
        assert_eq!(
            bom_match_url("https://api.diode.computer", true),
            "https://api.diode.computer/api/boms/match?strict=true"
        );
        assert_eq!(
            bom_match_url("https://api.diode.computer/", true),
            "https://api.diode.computer/api/boms/match?strict=true"
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
    fn schematic_hydration_respects_match_status() {
        let cases = [
            (
                "MATCH_COMPATIBLE",
                Some("API-MPN"),
                Some("API Manufacturer"),
                Some("https://example.com/API-MPN.pdf"),
            ),
            (
                "MATCH_EXACT",
                None,
                None,
                Some("https://example.com/API-MPN.pdf"),
            ),
            ("MATCH_FUZZY", None, None, None),
        ];

        for (status, expected_mpn, expected_manufacturer, expected_datasheet) in cases {
            let mut response = compatible_response();
            response["results"][0]["match"] = serde_json::json!(status);
            let response: MatchBomResponse = serde_json::from_value(response).unwrap();
            let mut schematic = test_schematic();
            prepare_bom_match(&schematic.bom(), &response)
                .unwrap()
                .apply_to_schematic(&mut schematic);

            let component = test_component(&schematic);
            assert_eq!(component.mpn().as_deref(), expected_mpn, "{status}");
            assert_eq!(
                component.manufacturer().as_deref(),
                expected_manufacturer,
                "{status}"
            );
            assert_eq!(
                component.string_attr(&["datasheet"]).as_deref(),
                expected_datasheet,
                "{status}"
            );
        }
    }

    #[test]
    fn hydration_does_not_replace_authored_metadata() {
        let response: MatchBomResponse = serde_json::from_value(compatible_response()).unwrap();
        let mut schematic = test_schematic();
        schematic
            .instances
            .values_mut()
            .next()
            .unwrap()
            .attributes
            .insert(
                "part".into(),
                AttributeValue::Json(serde_json::json!({
                    "mpn": "AUTHORED-MPN",
                    "manufacturer": "Authored Manufacturer",
                    "qualifications": [],
                    "datasheet": "https://example.com/authored.pdf"
                })),
            );
        let before = serde_json::to_value(&schematic).unwrap();

        prepare_bom_match(&schematic.bom(), &response)
            .unwrap()
            .apply_to_schematic(&mut schematic);
        assert_eq!(serde_json::to_value(&schematic).unwrap(), before);
    }

    #[test]
    fn incomplete_schematic_does_not_reach_panicking_bom_conversion() {
        let context = WorkspaceContext::from_api_base_url("https://api.example.com");
        let mut schematic = test_schematic();
        schematic
            .instances
            .values_mut()
            .next()
            .unwrap()
            .reference_designator = None;

        assert_eq!(
            hydrate_schematic_from_bom_with_cache(
                &context,
                &mut schematic,
                true,
                BomMatchMode::Offline,
                None,
                unix_now().unwrap(),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn schematic_hydration_refreshes_online_and_reuses_cache_offline() {
        let server = MockServer::start();
        let network = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true");
            then.status(200).json_body(compatible_response());
        });
        let context = WorkspaceContext::from_api_base_url(server.base_url());
        let tempdir = tempfile::tempdir().unwrap();
        let mut cache = cache_for(&tempdir);
        let now = unix_now().unwrap();

        for (mode, expected_mpn, expected_calls) in [
            (BomMatchMode::Offline, None, 0),
            (BomMatchMode::Online, Some("API-MPN"), 1),
            (BomMatchMode::Offline, Some("API-MPN"), 1),
        ] {
            let mut schematic = test_schematic();
            let hydrated = hydrate_schematic_from_bom_with_cache(
                &context,
                &mut schematic,
                true,
                mode,
                Some(&mut cache),
                now,
            )
            .unwrap();

            assert_eq!(hydrated, usize::from(expected_mpn.is_some()));
            assert_eq!(test_component(&schematic).mpn().as_deref(), expected_mpn);
            network.assert_calls(expected_calls);
        }
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
    fn identityless_entries_keep_path_specific_cache_groups() {
        let groups = bom_match_request_groups("https://example.com", &test_identityless_bom())
            .expect("identityless BOM should serialize");

        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|group| group.paths.len() == 1));
    }

    #[test]
    fn cache_identity_uses_sourcing_group_not_instances() {
        let first = bom_match_request_groups(
            "https://example.com/api/boms/match?strict=true",
            &test_bom_at("root.U1", "U1"),
        )
        .unwrap();
        let renamed = bom_match_request_groups(
            "https://example.com/api/boms/match?strict=true",
            &test_bom_at("nested.R99", "R99"),
        )
        .unwrap();
        let doubled = bom_match_request_groups(
            "https://example.com/api/boms/match?strict=true",
            &test_bom_with_second_line(),
        )
        .unwrap();

        assert_eq!(first[0].cache_key, renamed[0].cache_key);
        assert_ne!(first[0].cache_key, doubled[0].cache_key);
    }

    #[test]
    fn cache_policy_is_fresh_then_live_with_one_stale_fallback() {
        let server = MockServer::start();
        let mut success = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true");
            then.status(200).json_body(compatible_response());
        });
        let tempdir = tempfile::tempdir().unwrap();
        let mut cache = cache_for(&tempdir);
        let context = WorkspaceContext::from_api_base_url(server.base_url());
        let now = unix_now().unwrap();

        let mut offline_miss = test_bom();
        match_bom_with_cache(
            &context,
            None,
            &mut offline_miss,
            true,
            match_options(BomMatchMode::Offline),
            Some(&mut cache),
            now,
        )
        .unwrap();
        assert!(offline_miss.entries["root.U1"].mpn.is_none());
        success.assert_calls(0);

        let mut network = test_bom();
        match_bom_with_cache(
            &context,
            None,
            &mut network,
            true,
            match_options(BomMatchMode::Online),
            Some(&mut cache),
            now,
        )
        .unwrap();
        assert_eq!(network.entries["root.U1"].mpn.as_deref(), Some("API-MPN"));
        success.assert_calls(1);

        let mut fresh = test_bom_at("nested.R99", "R99");
        match_bom_with_cache(
            &context,
            None,
            &mut fresh,
            true,
            match_options(BomMatchMode::Online),
            Some(&mut cache),
            now + 100,
        )
        .unwrap();
        assert_eq!(fresh.entries["nested.R99"].mpn.as_deref(), Some("API-MPN"));
        assert_eq!(
            fresh.availability["nested.R99"]
                .selected_offer()
                .and_then(|offer| offer.datasheet_url.as_deref()),
            Some("https://example.com/API-MPN.pdf")
        );
        success.assert_calls(1);

        let mut custom_ttl = test_bom();
        match_bom_with_cache(
            &context,
            None,
            &mut custom_ttl,
            true,
            BomMatchOptions {
                mode: BomMatchMode::Online,
                cache_ttl: Duration::from_secs(10),
            },
            Some(&mut cache),
            now + 100,
        )
        .unwrap();
        assert_eq!(
            custom_ttl.entries["root.U1"].mpn.as_deref(),
            Some("API-MPN")
        );
        success.assert_calls(2);
        success.delete();

        let failure = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true");
            then.status(503).body("deploying");
        });
        let mut stale = test_bom();
        match_bom_with_cache(
            &context,
            None,
            &mut stale,
            true,
            match_options(BomMatchMode::Online),
            Some(&mut cache),
            now + 1_000,
        )
        .unwrap();
        assert_eq!(stale.entries["root.U1"].mpn.as_deref(), Some("API-MPN"));
        assert_eq!(
            stale.availability["root.U1"]
                .selected_offer()
                .and_then(|offer| offer.datasheet_url.as_deref()),
            Some("https://example.com/API-MPN.pdf")
        );
        failure.assert_calls(1);

        let mut offline = test_bom();
        match_bom_with_cache(
            &context,
            None,
            &mut offline,
            true,
            match_options(BomMatchMode::Offline),
            Some(&mut cache),
            now + 1_000,
        )
        .unwrap();
        assert_eq!(offline.entries["root.U1"].mpn.as_deref(), Some("API-MPN"));
        assert_eq!(
            offline.availability["root.U1"]
                .selected_offer()
                .and_then(|offer| offer.datasheet_url.as_deref()),
            Some("https://example.com/API-MPN.pdf")
        );
        failure.assert_calls(1);

        let mut no_cache = test_bom();
        assert!(
            match_bom_with_cache(
                &context,
                None,
                &mut no_cache,
                true,
                match_options(BomMatchMode::Online),
                None,
                now + 1_000,
            )
            .is_err()
        );
        assert!(no_cache.entries["root.U1"].mpn.is_none());
        failure.assert_calls(2);
    }

    #[test]
    fn completed_lines_are_served_and_cached_when_another_line_needs_retry() {
        let server = MockServer::start();
        let tempdir = tempfile::tempdir().unwrap();
        let mut cache = cache_for(&tempdir);
        let context = WorkspaceContext::from_api_base_url(server.base_url());
        let now = unix_now().unwrap();
        let mut bom = test_bom_with_distinct_second_line();
        let entries = bom_request_entries(&bom).unwrap();

        let mut initial_response = compatible_response();
        initial_response["results"]
            .as_array_mut()
            .unwrap()
            .push(retry_response("root.U2")["results"][0].clone());
        let initial = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true")
                .json_body(bom_match_request(&entries));
            then.status(200).json_body(initial_response);
        });

        match_bom_with_cache(
            &context,
            None,
            &mut bom,
            true,
            match_options(BomMatchMode::Online),
            Some(&mut cache),
            now,
        )
        .unwrap();
        assert_eq!(bom.entries["root.U1"].mpn.as_deref(), Some("API-MPN"));
        assert!(bom.entries["root.U2"].mpn.is_none());
        assert!(bom.availability.contains_key("root.U1"));
        assert!(!bom.availability.contains_key("root.U2"));

        let retry_entry = entries
            .into_iter()
            .find(|entry| entry["path"] == "root.U2")
            .unwrap();
        let retry = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true")
                .json_body(bom_match_request(&[retry_entry]));
            then.status(200).json_body(retry_response("root.U2"));
        });
        let mut cached = test_bom_with_distinct_second_line();
        match_bom_with_cache(
            &context,
            None,
            &mut cached,
            true,
            match_options(BomMatchMode::Online),
            Some(&mut cache),
            now + 1,
        )
        .unwrap();

        assert_eq!(cached.entries["root.U1"].mpn.as_deref(), Some("API-MPN"));
        assert!(cached.entries["root.U2"].mpn.is_none());
        assert!(cached.availability.contains_key("root.U1"));
        assert!(!cached.availability.contains_key("root.U2"));
        initial.assert_calls(1);
        retry.assert_calls(1);
    }

    #[test]
    fn cache_refreshes_identical_parts_as_one_planner_group() {
        let server = MockServer::start();
        let tempdir = tempfile::tempdir().unwrap();
        let mut cache = cache_for(&tempdir);
        let context = WorkspaceContext::from_api_base_url(server.base_url());
        let now = unix_now().unwrap();

        let one_line_bom = test_bom();
        let initial_request = bom_match_request(&bom_request_entries(&one_line_bom).unwrap());
        let initial = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true")
                .json_body(initial_request);
            then.status(200).json_body(compatible_response());
        });
        let mut initial_bom = one_line_bom;
        match_bom_with_cache(
            &context,
            None,
            &mut initial_bom,
            true,
            match_options(BomMatchMode::Online),
            Some(&mut cache),
            now,
        )
        .unwrap();
        initial.assert_calls(1);

        let mut partial_offline = test_bom_with_second_line();
        match_bom_with_cache(
            &context,
            None,
            &mut partial_offline,
            true,
            match_options(BomMatchMode::Offline),
            Some(&mut cache),
            now + 100,
        )
        .unwrap();
        assert!(partial_offline.entries["root.U1"].mpn.is_none());
        assert!(partial_offline.entries["root.U2"].mpn.is_none());
        initial.assert_calls(1);

        let grouped_bom = test_bom_with_second_line();
        let grouped_entries = bom_request_entries(&grouped_bom).unwrap();
        let refresh_request = bom_match_request(&grouped_entries);
        let mut failure = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true")
                .json_body(refresh_request.clone());
            then.status(503).body("offline");
        });
        let mut failed = test_bom_with_second_line();
        assert!(
            match_bom_with_cache(
                &context,
                None,
                &mut failed,
                true,
                match_options(BomMatchMode::Online),
                Some(&mut cache),
                now + 100,
            )
            .is_err()
        );
        assert!(failed.availability.is_empty());
        failure.assert_calls(1);
        failure.delete();

        let refresh = server.mock(|when, then| {
            when.method(POST)
                .path("/api/boms/match")
                .query_param("strict", "true")
                .json_body(refresh_request);
            then.status(200).json_body(compatible_response_for_lines(&[
                ("root.U1", "selected-offer", "API-MPN"),
                ("root.U2", "selected-offer", "API-MPN"),
            ]));
        });
        let mut online_bom = grouped_bom;
        match_bom_with_cache(
            &context,
            None,
            &mut online_bom,
            true,
            match_options(BomMatchMode::Online),
            Some(&mut cache),
            now + 100,
        )
        .unwrap();

        assert_eq!(
            online_bom.entries["root.U1"].mpn.as_deref(),
            Some("API-MPN")
        );
        assert_eq!(
            online_bom.entries["root.U2"].mpn.as_deref(),
            Some("API-MPN")
        );
        assert_eq!(
            online_bom.availability["root.U1"],
            online_bom.availability["root.U2"]
        );

        let mut offline = test_bom_with_second_line();
        match_bom_with_cache(
            &context,
            None,
            &mut offline,
            true,
            match_options(BomMatchMode::Offline),
            Some(&mut cache),
            now + 1_000,
        )
        .unwrap();
        assert_eq!(offline.availability.len(), 2);
        initial.assert_calls(1);
        refresh.assert_calls(1);
    }
}
