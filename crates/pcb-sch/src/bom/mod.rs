pub mod availability;
mod core;

// Re-export core BOM types
pub use core::*;

// Re-export availability types and helpers
pub use availability::{
    Availability, AvailabilitySummary, NUM_BOARDS, Offer, Tier, is_small_generic_passive,
    tier_for_stock,
};
