use pcb_sexpr::Sexpr;

use crate::{Symbol, SymbolDefinition};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerScope {
    Local,
    Global,
}

pub(crate) fn symbol_power_driver(
    symbol: &Symbol,
    definition: &SymbolDefinition,
) -> Option<(PowerScope, String)> {
    let power = definition.sexpr.find_list("power")?;
    let scope = match power.get(1).and_then(Sexpr::as_atom) {
        Some("local") => PowerScope::Local,
        Some("global") | None => PowerScope::Global,
        _ => return None,
    };
    let name = symbol
        .field_value("Value")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)?;
    Some((scope, name))
}
