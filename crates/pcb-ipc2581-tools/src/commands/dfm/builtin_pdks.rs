/// A bundled process definition, addressable by its exact `name`.
pub struct BuiltinPdk {
    pub name: &'static str,
    pub source: &'static str,
}

pub(super) const BUILTIN_PDKS: &[BuiltinPdk] = &[BuiltinPdk {
    name: "standard",
    source: include_str!("../../../pdks/standard.toml"),
}];

pub(super) fn find(name: &str) -> Option<&'static BuiltinPdk> {
    BUILTIN_PDKS.iter().find(|pdk| pdk.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_exact_references() {
        assert!(find("standard").is_some());
        assert!(find("./standard").is_none());
    }
}
