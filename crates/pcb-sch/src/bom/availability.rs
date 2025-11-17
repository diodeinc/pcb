/// BOM availability domain logic - tier classification and offer selection
use crate::GenericComponent;

/// Number of boards to use for availability and pricing calculations
pub const NUM_BOARDS: i32 = 20;

/// Availability tier for sourcing status
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
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
