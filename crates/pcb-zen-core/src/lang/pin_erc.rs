use std::collections::BTreeSet;

use crate::lang::symbol::SymbolValue;

pub(crate) fn signal_pin_type_candidates(symbol: &SymbolValue, signal_name: &str) -> Vec<String> {
    let pad_numbers: BTreeSet<&str> = symbol
        .pad_to_signal()
        .iter()
        .filter_map(|(pad, signal)| (signal == signal_name).then_some(pad.as_str()))
        .collect();

    if pad_numbers.is_empty() {
        return Vec::new();
    }

    let mut candidates = BTreeSet::new();
    for pin in symbol
        .pins()
        .iter()
        .filter(|pin| pad_numbers.contains(pin.number.as_str()))
    {
        if let Some(pin_type) = pin.electrical_type.as_deref() {
            candidates.insert(pin_type.to_string());
        }
        for alternate in &pin.alternates {
            if let Some(pin_type) = alternate.electrical_type.as_deref() {
                candidates.insert(pin_type.to_string());
            }
        }
    }

    candidates.into_iter().collect()
}

pub(crate) fn pin_types_are_only_no_connect(pin_types: &[String]) -> bool {
    !pin_types.is_empty() && pin_types.iter().all(|pin_type| pin_type == "no_connect")
}

pub(crate) fn pin_no_connect_body(
    component_name: &str,
    signal_name: &str,
    net_kind: &str,
    net_name: &str,
) -> String {
    format!(
        "Pin '{signal_name}' on component '{component_name}' is marked no_connect but was explicitly connected to {net_kind} net '{net_name}'; omit it from `pins` and Component() will wire NotConnected() automatically"
    )
}
