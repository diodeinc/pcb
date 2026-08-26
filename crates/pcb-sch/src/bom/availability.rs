//! BOM availability types.

use serde::{Deserialize, Serialize};

/// Board quantity sent to the sourcing planner and used for price presentation.
pub const BOARD_QUANTITY: i32 = 5;

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
    /// MPN from the offer (internal only)
    #[serde(skip, default)]
    pub mpn: Option<String>,
    /// Manufacturer from the offer (internal only)
    #[serde(skip, default)]
    pub manufacturer: Option<String>,
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
}
