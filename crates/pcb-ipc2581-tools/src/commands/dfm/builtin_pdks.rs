/// A bundled process definition, addressable by its exact `name`.
#[derive(serde::Serialize)]
pub struct BuiltinPdk {
    pub name: &'static str,
    pub profile: &'static str,
    pub source: &'static str,
}

const STANDARD: &str = include_str!("../../../pdks/standard.toml");
const IPC: &str = include_str!("../../../pdks/ipc.toml");
const JLCPCB: &str = include_str!("../../../pdks/jlcpcb.toml");

macro_rules! builtin {
    ($name:literal, $profile:literal, $source:expr) => {
        BuiltinPdk {
            name: $name,
            profile: $profile,
            source: $source,
        }
    };
}

pub(super) const BUILTIN_PDKS: &[BuiltinPdk] = &[
    builtin!("standard", "standard", STANDARD),
    builtin!("jlcpcb-1oz", "one-ounce", JLCPCB),
    builtin!("jlc", "one-ounce", JLCPCB),
    builtin!("ipc", "2b", IPC),
    builtin!("ipc-1a", "1a", IPC),
    builtin!("ipc-1b", "1b", IPC),
    builtin!("ipc-1c", "1c", IPC),
    builtin!("ipc-2a", "2a", IPC),
    builtin!("ipc-2b", "2b", IPC),
    builtin!("ipc-2c", "2c", IPC),
    builtin!("ipc-3a", "3a", IPC),
    builtin!("ipc-3b", "3b", IPC),
    builtin!("ipc-3c", "3c", IPC),
];

pub(super) fn find(name: &str) -> Option<&'static BuiltinPdk> {
    BUILTIN_PDKS.iter().find(|pdk| pdk.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_exact_references() {
        assert!(find("standard").is_some());
        assert!(find("jlcpcb-1oz").is_some());
        assert!(find("jlc").is_some());
        assert_eq!(find("ipc").unwrap().profile, "2b");
        assert!(find("ipc-3c").is_some());
        assert!(find("./standard").is_none());
    }
}
