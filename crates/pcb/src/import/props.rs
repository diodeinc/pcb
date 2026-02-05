use super::ImportComponentData;
use std::collections::BTreeMap;
use std::sync::OnceLock;

fn empty_properties() -> &'static BTreeMap<String, String> {
    static EMPTY: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    EMPTY.get_or_init(BTreeMap::new)
}

pub(super) fn best_properties(component: &ImportComponentData) -> &BTreeMap<String, String> {
    if let Some(sch) = &component.schematic {
        if let Some(unit) = sch.units.values().next() {
            return &unit.properties;
        }
    }
    if let Some(layout) = component.layout.as_ref() {
        return &layout.properties;
    }
    empty_properties()
}

pub(super) fn find_property_ci<'a>(
    props: &'a BTreeMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    for want in keys {
        for (k, v) in props {
            if k.eq_ignore_ascii_case(want) {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
    }
    None
}
