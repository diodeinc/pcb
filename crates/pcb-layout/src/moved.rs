//! Apply moved() path renames to KiCad PCB files.
//!
//! This module handles preprocessing of .kicad_pcb files to apply path renames
//! before the main sync process runs. This is a structural operation that:
//! 1. Walks the parsed board to find patchable strings using structural predicates
//! 2. Applies longest-prefix matching to determine renames
//! 3. Returns patches that can be applied while preserving formatting
//! 4. Updates footprint UUIDs to match the new paths
//!
//! Also provides `compute_net_renames_patches` for implicit net rename detection,
//! which uses exact-match only and patches only net-related strings.

use pcb_sexpr::board::{
    is_footprint_kiid_path, is_footprint_path_property, is_group_name, is_net_name,
    is_zone_net_name,
};
use pcb_sexpr::{PatchSet, Sexpr, WalkCtx};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// UUID namespace used for generating deterministic footprint UUIDs from paths.
/// This matches Python: uuid.NAMESPACE_URL
const UUID_NAMESPACE_URL: Uuid = Uuid::from_u128(0x6ba7b811_9dad_11d1_80b4_00c04fd430c8);

/// Compute patches for net-only renames (exact match, no prefix matching).
///
/// This is used for implicit net rename detection where we only want to rename
/// net declarations and zone net_names, NOT footprint paths or group names.
pub fn compute_net_renames_patches(
    board: &Sexpr,
    net_renames: &HashMap<String, String>,
) -> (PatchSet, Vec<(String, String)>) {
    let mut patches = PatchSet::default();
    let mut renames = Vec::new();

    if net_renames.is_empty() {
        return (patches, renames);
    }

    let is_net_patchable = |ctx: &WalkCtx<'_>| is_net_name(ctx) || is_zone_net_name(ctx);

    // Collect existing net names to prevent collisions
    let mut existing: HashSet<String> = HashSet::new();
    board.walk_strings(|value, _span, ctx| {
        if is_net_patchable(&ctx) {
            existing.insert(value.to_string());
        }
    });

    // Apply exact-match renames
    board.walk_strings(|value, span, ctx| {
        if is_net_patchable(&ctx)
            && let Some(new_value) = net_renames.get(value)
            && !existing.contains(new_value)
        {
            patches.replace_string(span, new_value);
            renames.push((value.to_string(), new_value.clone()));
        }
    });

    (patches, renames)
}

/// Compute patches for moved() path renames on a board.
///
/// Takes the parsed board and a map of old->new path prefixes.
/// Returns patches to apply and a list of (old, new) renames that were applied.
///
/// Uses longest-prefix matching:
/// - For a path like "Power.R1" and moved_paths {"Power": "Supply"},
///   the result is "Supply.R1"
///
/// Also updates footprint UUIDs to match the new paths.
pub fn compute_moved_paths_patches(
    board: &Sexpr,
    moved_paths: &HashMap<String, String>,
) -> (PatchSet, Vec<(String, String)>) {
    let mut patches = PatchSet::default();
    let mut renames = Vec::new();

    if moved_paths.is_empty() {
        return (patches, renames);
    }

    // Helper: check if context is a patchable identifier (footprint path, group name, net name, or zone net_name)
    let is_patchable = |ctx: &WalkCtx<'_>| {
        is_footprint_path_property(ctx)
            || is_group_name(ctx)
            || is_net_name(ctx)
            || is_zone_net_name(ctx)
    };

    // First pass: collect existing identifiers
    let mut existing: HashSet<String> = HashSet::new();
    board.walk_strings(|value, _span, ctx| {
        if is_patchable(&ctx) {
            existing.insert(value.to_string());
        }
    });

    // Second pass: apply renames, skipping if computed target already exists
    let mut footprint_path_renames: HashMap<String, String> = HashMap::new();
    board.walk_strings(|value, span, ctx| {
        if let Some(new_value) = apply_longest_prefix_match(value, moved_paths) {
            // Skip if computed target already exists (idempotency / collision safety)
            if is_patchable(&ctx) && !existing.contains(&new_value) {
                patches.replace_string(span, &new_value);
                renames.push((value.to_string(), new_value.clone()));
                if is_footprint_path_property(&ctx) {
                    footprint_path_renames.insert(value.to_string(), new_value);
                }
            }
        }
    });

    // Second pass: update footprint KiCad UUIDs based on old UUID -> new path mapping
    // We need to find (path "/old-uuid") entries and compute new UUIDs from the new paths
    if !footprint_path_renames.is_empty() {
        // Build a map of old_uuid -> new_uuid
        let mut uuid_renames: HashMap<String, String> = HashMap::new();
        for (old_path, new_path) in &footprint_path_renames {
            let old_uuid = compute_uuid_from_path(old_path);
            let new_uuid = compute_uuid_from_path(new_path);
            uuid_renames.insert(old_uuid, new_uuid);
        }

        // Walk again to find and patch UUID paths
        board.walk_strings(|value, span, ctx| {
            if is_footprint_kiid_path(&ctx) {
                // value is like "/uuid" or "/uuid/uuid"
                let trimmed = value.trim_start_matches('/');
                // Extract the first UUID segment
                let first_uuid = trimmed.split('/').next().unwrap_or(trimmed);
                if let Some(new_uuid) = uuid_renames.get(first_uuid) {
                    // Rebuild the path with new UUID (format: /uuid/uuid)
                    let new_kiid_path = format!("/{new_uuid}/{new_uuid}");
                    patches.replace_string(span, &new_kiid_path);
                }
            }
        });
    }

    (patches, renames)
}

/// Compute deterministic UUID from a hierarchical path.
/// Uses UUID v5 with NAMESPACE_URL, matching Python's uuid.uuid5(uuid.NAMESPACE_URL, path).
fn compute_uuid_from_path(path: &str) -> String {
    Uuid::new_v5(&UUID_NAMESPACE_URL, path.as_bytes()).to_string()
}

/// Apply longest-prefix matching to remap a path.
///
/// Given a path like "Power.R1" and moved_paths {"Power": "Supply"},
/// returns Some("Supply.R1").
///
/// If no prefix matches, returns None.
fn apply_longest_prefix_match(path: &str, moved_paths: &HashMap<String, String>) -> Option<String> {
    let mut best_match: Option<(&str, &str)> = None;
    let mut best_len = 0;

    for (old_prefix, new_prefix) in moved_paths {
        if path == old_prefix {
            return Some(new_prefix.clone());
        } else if path.starts_with(old_prefix) {
            let rest = &path[old_prefix.len()..];
            if rest.starts_with('.') && old_prefix.len() > best_len {
                best_match = Some((old_prefix.as_str(), new_prefix.as_str()));
                best_len = old_prefix.len();
            }
        }
    }

    best_match.map(|(old_prefix, new_prefix)| {
        let suffix = &path[old_prefix.len()..];
        format!("{new_prefix}{suffix}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_sexpr::parse;

    fn apply_to_string(
        board: &Sexpr,
        source: &str,
        moved_paths: &HashMap<String, String>,
    ) -> (String, Vec<(String, String)>) {
        let (patches, renames) = compute_moved_paths_patches(board, moved_paths);
        let mut buf = Vec::new();
        patches.write_to(source, &mut buf).unwrap();
        (String::from_utf8(buf).unwrap(), renames)
    }

    #[test]
    fn test_longest_prefix_match() {
        let mut moved = HashMap::new();
        moved.insert("Power".to_string(), "Supply".to_string());
        moved.insert("Power.Sub".to_string(), "Supply.Module".to_string());

        assert_eq!(
            apply_longest_prefix_match("Power", &moved),
            Some("Supply".to_string())
        );
        assert_eq!(
            apply_longest_prefix_match("Power.R1", &moved),
            Some("Supply.R1".to_string())
        );
        assert_eq!(
            apply_longest_prefix_match("Power.Sub.R1", &moved),
            Some("Supply.Module.R1".to_string())
        );
        assert_eq!(apply_longest_prefix_match("Signal.R1", &moved), None);
        assert_eq!(apply_longest_prefix_match("PowerSupply.R1", &moved), None);
    }

    #[test]
    fn test_uuid_computation() {
        // Verify UUID computation matches Python's uuid.uuid5(uuid.NAMESPACE_URL, path)
        let uuid = compute_uuid_from_path("Power.R1");
        // This should be deterministic
        assert_eq!(uuid.len(), 36); // UUID format: 8-4-4-4-12
        assert!(uuid.contains('-'));

        // Same path should produce same UUID
        assert_eq!(uuid, compute_uuid_from_path("Power.R1"));

        // Different paths should produce different UUIDs
        assert_ne!(uuid, compute_uuid_from_path("Power.R2"));
    }

    #[test]
    fn test_apply_moved_paths() {
        let input = r#"(kicad_pcb
            (net 1 "Power_VCC")
            (footprint "R_0603"
                (property "Path" "Power.R1.R")
            )
            (group "Power"
                (uuid "123")
            )
        )"#;

        let board = parse(input).unwrap();

        let mut moved = HashMap::new();
        moved.insert("Power".to_string(), "Supply".to_string());

        let (result, renames) = apply_to_string(&board, input, &moved);

        assert!(result.contains("\"Supply.R1.R\""));
        assert!(!result.contains("\"Power.R1.R\""));
        assert!(result.contains("(group \"Supply\""));
        assert!(!result.contains("(group \"Power\""));
        assert!(result.contains("\"Power_VCC\"")); // Net unchanged

        assert_eq!(renames.len(), 2); // footprint path + group name
    }

    #[test]
    fn test_apply_moved_paths_with_uuid() {
        // Compute the expected UUIDs
        let old_uuid = compute_uuid_from_path("Power.R1");
        let new_uuid = compute_uuid_from_path("Supply.R1");

        let input = format!(
            r#"(kicad_pcb
            (footprint "R_0603"
                (path "/{old_uuid}/{old_uuid}")
                (property "Path" "Power.R1")
            )
        )"#
        );

        let board = parse(&input).unwrap();

        let mut moved = HashMap::new();
        moved.insert("Power".to_string(), "Supply".to_string());

        let (result, renames) = apply_to_string(&board, &input, &moved);

        // Path property should be updated
        assert!(result.contains("\"Supply.R1\""));
        assert!(!result.contains("\"Power.R1\""));

        // UUID path should be updated
        assert!(result.contains(&format!("\"/{new_uuid}/{new_uuid}\"")));
        assert!(!result.contains(&format!("\"/{old_uuid}/{old_uuid}\"")));

        assert_eq!(renames.len(), 1);
    }

    #[test]
    fn test_preserves_formatting() {
        let input = r#"(kicad_pcb
	(version 20241229)
	(footprint "R_0603"
		(property "Path" "Old.Path"
			(at 0 0 0)
		)
	)
)"#;

        let board = parse(input).unwrap();

        let mut moved = HashMap::new();
        moved.insert("Old".to_string(), "New".to_string());

        let (result, _) = apply_to_string(&board, input, &moved);

        assert!(result.contains("(version 20241229)"));
        assert!(result.contains("\t(footprint"));
        assert!(result.contains("\t\t(property \"Path\" \"New.Path\""));
        assert!(result.contains("\t\t\t(at 0 0 0)"));
    }

    #[test]
    fn test_net_exact_match() {
        let input = r#"(kicad_pcb
            (net 1 "OLD_VCC")
            (net 2 "OLD_GND")
        )"#;

        let board = parse(input).unwrap();

        let mut moved = HashMap::new();
        moved.insert("OLD_VCC".to_string(), "NEW_VCC".to_string());
        moved.insert("OLD_GND".to_string(), "NEW_GND".to_string());

        let (result, renames) = apply_to_string(&board, input, &moved);

        assert!(result.contains("\"NEW_VCC\""));
        assert!(result.contains("\"NEW_GND\""));
        assert_eq!(renames.len(), 2);
    }

    #[test]
    fn test_skip_rename_when_target_exists() {
        // If computed target already exists, skip that specific rename.
        // Covers: idempotency (already renamed) and collision prevention.

        // Case 1: Computed path collision - "Old.R1" would become "New.R1" but it already exists
        let input = r#"(kicad_pcb
            (footprint "R_0603"
                (property "Path" "Old.R1")
            )
            (footprint "R_0603"
                (property "Path" "New.R1")
            )
        )"#;

        let board = parse(input).unwrap();
        let mut moved = HashMap::new();
        moved.insert("Old".to_string(), "New".to_string());

        let (result, renames) = apply_to_string(&board, input, &moved);

        // "New.R1" already exists, so Old.R1 -> New.R1 rename is skipped
        assert!(result.contains("\"Old.R1\""));
        assert!(result.contains("\"New.R1\""));
        assert_eq!(renames.len(), 0);

        // Case 2: Exact target match - group rename skipped
        let input2 = r#"(kicad_pcb
            (group "OldGroup"
                (uuid "123")
            )
            (group "NewGroup"
                (uuid "456")
            )
        )"#;

        let board2 = parse(input2).unwrap();
        let mut moved2 = HashMap::new();
        moved2.insert("OldGroup".to_string(), "NewGroup".to_string());

        let (result2, renames2) = apply_to_string(&board2, input2, &moved2);

        assert!(result2.contains("\"OldGroup\""));
        assert!(result2.contains("\"NewGroup\""));
        assert_eq!(renames2.len(), 0);
    }

    #[test]
    fn test_zone_net_name_rename() {
        let input = r#"(kicad_pcb
            (net 1 "gnd")
            (zone
                (net 1)
                (net_name "gnd")
                (layer "F.Cu")
            )
        )"#;

        let board = parse(input).unwrap();

        let mut moved = HashMap::new();
        moved.insert("gnd".to_string(), "GND".to_string());

        let (result, renames) = apply_to_string(&board, input, &moved);

        // Both net declaration and zone net_name should be updated
        assert!(result.contains("(net 1 \"GND\")"));
        assert!(result.contains("(net_name \"GND\")"));
        assert!(!result.contains("\"gnd\""));
        assert_eq!(renames.len(), 2); // net + zone net_name
    }

    #[test]
    fn test_net_only_rename_does_not_touch_footprint_paths() {
        // Regression test: compute_net_renames_patches must NOT rename footprint paths or groups
        let input = r#"(kicad_pcb
            (net 1 "Power")
            (group "Power"
                (uuid "123")
            )
            (footprint "R_0603"
                (property "Path" "Power.R1")
            )
            (zone
                (net 1)
                (net_name "Power")
            )
        )"#;

        let board = parse(input).unwrap();

        let mut renames = HashMap::new();
        renames.insert("Power".to_string(), "Supply".to_string());

        let (patches, applied) = super::compute_net_renames_patches(&board, &renames);
        let mut buf = Vec::new();
        patches.write_to(input, &mut buf).unwrap();
        let result = String::from_utf8(buf).unwrap();

        // Net and zone net_name SHOULD be renamed
        assert!(result.contains("(net 1 \"Supply\")"));
        assert!(result.contains("(net_name \"Supply\")"));

        // Footprint path and group MUST NOT be renamed
        assert!(result.contains("\"Power.R1\""));
        assert!(result.contains("(group \"Power\""));

        assert_eq!(applied.len(), 2); // only net + zone net_name
    }
}
