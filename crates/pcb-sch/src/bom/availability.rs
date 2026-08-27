//! BOM availability types.

use serde::{Deserialize, Serialize};

/// Board quantity sent to the sourcing planner and used for price presentation.
pub const BOARD_QUANTITY: i32 = 5;

/// Match result returned by the BOM service.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BomMatchStatus {
    #[serde(rename = "MATCH_EXACT")]
    Exact,
    #[serde(rename = "MATCH_COMPATIBLE")]
    Compatible,
    #[serde(rename = "MATCH_FUZZY")]
    Fuzzy,
    #[serde(rename = "MATCH_NEEDS_RETRY")]
    NeedsRetry,
    #[serde(rename = "MATCH_FAILED")]
    Failed,
}

/// Preferred-part collection assigned by the BOM service.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PartCollection {
    House,
    Extended,
}

/// Stock classification computed by the API sourcing planner.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SourcingStockClass {
    Plenty,
    Limited,
    Insufficient,
    #[default]
    Unknown,
}

/// Pricing and availability data for a component
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Availability {
    /// How the BOM service matched this line.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_status: Option<BomMatchStatus>,
    /// Best US availability summary (price @ stock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us: Option<AvailabilitySummary>,
    /// Best Global availability summary (price @ stock)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<AvailabilitySummary>,
    /// The matching service found no component for the specified MPN.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub no_match: bool,
    /// Offer selected by the API sourcing planner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_offer_id: Option<String>,
    /// All raw offers for detailed display
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub offers: Vec<Offer>,
}

impl Availability {
    pub fn selected_offer(&self) -> Option<&Offer> {
        let selected_offer_id = self.selected_offer_id.as_deref()?;
        self.offers
            .iter()
            .find(|offer| offer.id.as_deref() == Some(selected_offer_id))
    }

    pub fn selected_part_collection(&self) -> Option<PartCollection> {
        let collections = &self.selected_offer()?.part_collections;
        if collections.contains(&PartCollection::House) {
            Some(PartCollection::House)
        } else if collections.contains(&PartCollection::Extended) {
            Some(PartCollection::Extended)
        } else {
            None
        }
    }

    pub fn compatible_part(&self) -> Option<(&str, &str)> {
        if self.match_status != Some(BomMatchStatus::Compatible) {
            return None;
        }

        let offer = self.selected_offer()?;
        Some((
            offer.mpn.as_deref()?.trim(),
            offer.manufacturer.as_deref()?.trim(),
        ))
        .filter(|(mpn, manufacturer)| !mpn.is_empty() && !manufacturer.is_empty())
    }

    pub fn selected_datasheet_url(&self) -> Option<&str> {
        if !matches!(
            self.match_status,
            Some(BomMatchStatus::Exact | BomMatchStatus::Compatible)
        ) {
            return None;
        }

        self.selected_offer()?
            .datasheet_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
    }
}

/// Compact availability summary for a region
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AvailabilitySummary {
    /// API planner classification for the selected offer (internal only)
    #[serde(skip, default)]
    pub stock_class: SourcingStockClass,
    /// Unit price at target quantity
    pub price: Option<f64>,
    /// Stock available (best offer)
    pub stock: i32,
    /// Combined stock from alternative offers
    pub alt_stock: i32,
    /// Price breaks for computing prices at different quantities (internal only)
    #[serde(skip, default)]
    pub price_breaks: Option<Vec<(i32, f64)>>,
    /// LCSC part IDs for hyperlinks (internal only)
    #[serde(skip, default)]
    pub lcsc_part_ids: Vec<(String, String)>,
}

/// Distributor offer with live pricing/stock data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Offer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub region: String,
    pub distributor: String,
    pub stock: i32,
    pub price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datasheet_url: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub part_collections: Vec<PartCollection>,
}
