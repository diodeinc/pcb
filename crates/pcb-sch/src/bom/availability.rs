//! BOM availability types and domain logic.
//!
//! Contains both the JSON-facing availability types (shared between BOM JSON output
//! and MCP/search results) and domain logic for tier classification and offer selection.

use serde::{Deserialize, Serialize};

use super::GenericComponent;

/// Number of boards to use for availability and pricing calculations
pub const NUM_BOARDS: i32 = 20;

/// Availability tier for sourcing status
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Tier {
    #[default]
    Insufficient = 0,
    Limited = 1,
    Plenty = 2,
}

impl Tier {
    /// Rank for comparisons (lower is better)
    #[inline]
    pub fn rank(self) -> u8 {
        match self {
            Tier::Plenty => 0,
            Tier::Limited => 1,
            Tier::Insufficient => 2,
        }
    }
}

/// Check if component is a small generic passive requiring higher stock threshold
pub fn is_small_generic_passive(
    generic_data: Option<&GenericComponent>,
    package: Option<&str>,
) -> bool {
    let is_generic_passive = matches!(
        generic_data,
        Some(GenericComponent::Resistor(_) | GenericComponent::Capacitor(_))
    );
    let is_small_package = matches!(package, Some("0201" | "0402" | "0603"));

    is_generic_passive && is_small_package
}

/// Determine availability tier based on stock and quantity
pub fn tier_for_stock(stock: i32, qty: i32, is_small_passive: bool) -> Tier {
    // Red tier: not enough for even 1 board
    if stock < qty {
        return Tier::Insufficient;
    }

    // Green tier: enough for NUM_BOARDS or 100 for small passives
    let required_stock = if is_small_passive {
        100
    } else {
        qty * NUM_BOARDS
    };

    if stock >= required_stock {
        Tier::Plenty
    } else {
        Tier::Limited
    }
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
    /// All raw offers for detailed display
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub offers: Vec<Offer>,
}

/// Compact availability summary for a region
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AvailabilitySummary {
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
    pub region: String,
    pub distributor: String,
    pub stock: i32,
    pub price: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_id: Option<String>,
}
