pub mod availability;
mod core;

// Re-export core BOM types
pub use core::*;

// Re-export availability types and sourcing quantity
pub use availability::{
    Availability, AvailabilitySummary, BOARD_QUANTITY, BomMatchStatus, Offer, PartCollection,
    SourcingStockClass,
};
