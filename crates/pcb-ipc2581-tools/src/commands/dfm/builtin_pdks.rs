/// A bundled process definition, addressable by its exact `name`. A name
/// without a `profile` selects the kit's profile from the design.
#[derive(serde::Serialize)]
pub struct BuiltinPdk {
    pub name: &'static str,
    pub profile: Option<&'static str>,
    pub source: &'static str,
}

const STANDARD: &str = include_str!("../../../pdks/standard.toml");
const IPC: &str = include_str!("../../../pdks/ipc.toml");
const JLCPCB: &str = concat!(
    include_str!("../../../pdks/jlcpcb.toml"),
    "\n",
    include_str!("../../../pdks/jlcpcb-stackups.toml"),
);

macro_rules! builtin {
    ($name:literal, $profile:literal, $source:expr) => {
        BuiltinPdk {
            name: $name,
            profile: Some($profile),
            source: $source,
        }
    };
    ($name:literal, $source:expr) => {
        BuiltinPdk {
            name: $name,
            profile: None,
            source: $source,
        }
    };
}

pub(super) const BUILTIN_PDKS: &[BuiltinPdk] = &[
    builtin!("standard", STANDARD),
    builtin!("jlcpcb-1oz", "one-ounce", JLCPCB),
    builtin!("jlc", JLCPCB),
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
