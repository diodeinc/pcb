//! Shared KiCad identity rules for schematic symbols and PCB footprints.

use uuid::Uuid;

/// Python's `uuid.NAMESPACE_URL`, used by the KiCad layout sync implementation.
pub const UUID_NAMESPACE_URL: Uuid = Uuid::from_u128(0x6ba7b811_9dad_11d1_80b4_00c04fd430c8);

/// Generate the stable KiCad UUID for a canonical component path.
pub fn uuid_for_path(path: &str) -> String {
    Uuid::new_v5(&UUID_NAMESPACE_URL, path.as_bytes()).to_string()
}

/// Generate the KIID path stored on a managed PCB footprint.
pub fn footprint_kiid_path(path: &str) -> String {
    let uuid = uuid_for_path(path);
    format!("/{uuid}/{uuid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_existing_layout_identity_contract() {
        assert_eq!(
            uuid_for_path("Power.R1"),
            "af22c38c-01f1-5f54-bffa-a866b8a62586"
        );
        assert_eq!(
            footprint_kiid_path("Power.R1"),
            "/af22c38c-01f1-5f54-bffa-a866b8a62586/af22c38c-01f1-5f54-bffa-a866b8a62586"
        );
    }
}
