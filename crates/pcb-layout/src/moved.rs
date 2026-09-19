//! Apply path renames to KiCad PCB files.
//!
//! This module handles preprocessing of .kicad_pcb files to apply path renames
//! before the main sync process runs. This is a structural operation that:
//! 1. Walks the parsed board to find patchable strings using structural predicates
//! 2. Renames them, skipping any rename whose target already exists
//! 3. Returns patches that can be applied while preserving formatting
//! 4. Updates footprint KIID paths to match the new paths
//!
//! `compute_moved_paths_patches` applies moved() directives by longest-prefix match.
//! `compute_net_renames_patches` and `compute_truncated_path_patches` use exact-match only,
//! and patch only net-related strings and only footprint paths and group names respectively.

use pcb_sch::kicad_identity::{footprint_kiid_path, uuid_for_path};
use pcb_sch::{InstanceKind, Schematic};
use pcb_sexpr::board::{
    is_footprint_kiid_path, is_footprint_path_property, is_group_name, is_net_name,
    is_zone_net_name,
};
use pcb_sexpr::{PatchSet, Sexpr, WalkCtx};
use std::collections::{HashMap, HashSet};

/// Compute patches for net-only renames (exact match, no prefix matching).
///
/// This is used for implicit net rename detection where we only want to rename
/// net references, NOT footprint paths or group names.
pub fn compute_net_renames_patches(
    board: &Sexpr,
    net_renames: &HashMap<String, String>,
) -> (PatchSet, Vec<(String, String)>) {
    compute_path_patches(
        board,
        |path| net_renames.get(path).cloned(),
        |ctx| is_net_name(ctx) || is_zone_net_name(ctx),
    )
}

/// Compute patches for moved() path renames on a board.
///
/// Takes the parsed board and a map of old->new path prefixes.
/// Returns patches to apply and a list of (old, new) renames that were applied.
///
/// Uses longest-prefix matching:
/// - For a path like "Power.R1" and moved_paths {"Power": "Supply"},
///   the result is "Supply.R1"
pub fn compute_moved_paths_patches(
    board: &Sexpr,
    moved_paths: &HashMap<String, String>,
) -> (PatchSet, Vec<(String, String)>) {
    compute_path_patches(
        board,
        |path| apply_longest_prefix_match(path, moved_paths),
        |ctx| {
            is_footprint_path_property(ctx)
                || is_group_name(ctx)
                || is_net_name(ctx)
                || is_zone_net_name(ctx)
        },
    )
}

/// Map the paths an older layout sync truncated to the full paths they should carry.
///
/// Older syncs kept only the text after a path's last colon, so "Block:Power.R1" was
/// stored as "Power.R1". A truncated path is restored when it names nothing in the
/// source and exactly one instance truncates to it.
pub(crate) fn truncated_paths(schematic: &Schematic) -> HashMap<String, String> {
    let paths: HashSet<String> = schematic
        .instances
        .iter()
        .filter(|(_, instance)| {
            matches!(
                instance.kind,
                InstanceKind::Component | InstanceKind::Module
            )
        })
        .map(|(instance_ref, _)| instance_ref.instance_path.join("."))
        .collect();

    let mut full_paths: HashMap<&str, Vec<&str>> = HashMap::new();
    paths
        .iter()
        .filter_map(|path| Some((path.rsplit_once(':')?.1, path.as_str())))
        .filter(|(truncated, _)| !paths.contains(*truncated))
        .for_each(|(truncated, path)| full_paths.entry(truncated).or_default().push(path));

    full_paths
        .into_iter()
        .filter_map(|(truncated, full_paths)| match full_paths.as_slice() {
            [full_path] => Some((truncated.to_string(), full_path.to_string())),
            _ => None,
        })
        .collect()
}

/// Compute patches restoring truncated footprint paths and group names (exact match).
pub(crate) fn compute_truncated_path_patches(
    board: &Sexpr,
    truncated_paths: &HashMap<String, String>,
) -> (PatchSet, Vec<(String, String)>) {
    compute_path_patches(
        board,
        |path| truncated_paths.get(path).cloned(),
        |ctx| is_footprint_path_property(ctx) || is_group_name(ctx),
    )
}

/// Compute patches renaming every patchable string that `rename` maps to a new value.
///
/// Returns patches to apply and a list of (old, new) renames that were applied.
/// Renamed footprints keep their UUIDs; only their KIID paths follow the new paths.
fn compute_path_patches(
    board: &Sexpr,
    rename: impl Fn(&str) -> Option<String>,
    is_patchable: impl Fn(&WalkCtx<'_>) -> bool,
) -> (PatchSet, Vec<(String, String)>) {
    let mut patches = PatchSet::default();
    let mut renames = Vec::new();

    // First pass: collect existing identifiers
    let mut existing: HashSet<String> = HashSet::new();
    board.walk_strings(|value, _span, ctx| {
        if is_patchable(&ctx) {
            existing.insert(value.to_string());
        }
    });

    // Second pass: apply renames, skipping if computed target already exists
    // (idempotency / collision safety)
    let mut kiid_path_renames: HashMap<String, String> = HashMap::new();
    board.walk_strings(|value, span, ctx| {
        if is_patchable(&ctx)
            && let Some(new_value) = rename(value)
            && !existing.contains(&new_value)
        {
            patches.replace_string(span, &new_value);
            if is_footprint_path_property(&ctx) {
                kiid_path_renames.insert(uuid_for_path(value), footprint_kiid_path(&new_value));
            }
            renames.push((value.to_string(), new_value));
        }
    });

    // Third pass: point (path "/old-uuid/old-uuid") entries at the renamed footprint paths
    board.walk_strings(|value, span, ctx| {
        if is_footprint_kiid_path(&ctx) {
            // value is like "/uuid" or "/uuid/uuid"
            let trimmed = value.trim_start_matches('/');
            let first_uuid = trimmed.split('/').next().unwrap_or(trimmed);
            if let Some(new_kiid_path) = kiid_path_renames.get(first_uuid) {
                patches.replace_string(span, new_kiid_path);
            }
        }
    });

    (patches, renames)
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
    use pcb_sch::{Instance, InstanceRef, ModuleRef};
    use pcb_sexpr::parse;

    #[test]
    fn test_truncated_paths() {
        let module = ModuleRef::new("board.zen", "<root>");
        let truncated = |paths: &[&str]| {
            let mut schematic = Schematic::new();
            for path in paths {
                schematic.add_instance(
                    InstanceRef::new(module.clone(), path.split('.').map(Into::into).collect()),
                    Instance::component(module.clone()),
                );
            }
            truncated_paths(&schematic)
        };

        assert_eq!(
            truncated(&["Signal.R1", "Block:Power.R1", "Block:Power.Chip:IC"]),
            HashMap::from([
                ("Power.R1".to_string(), "Block:Power.R1".to_string()),
                ("IC".to_string(), "Block:Power.Chip:IC".to_string()),
            ])
        );
        // "Power.R1" names a source instance, so "Block:Power.R1" must not claim its footprint.
        assert!(truncated(&["Power.R1", "Block:Power.R1"]).is_empty());
        // Two instances truncate to "Power.R1".
        assert!(truncated(&["Block:Power.R1", "Other:Power.R1"]).is_empty());
    }

    #[test]
    fn test_restore_truncated_paths() {
        let old_kiid_path = footprint_kiid_path("Power.R1");
        let new_kiid_path = footprint_kiid_path("Block:Power.R1");
        let input = format!(
            r#"(kicad_pcb
            (net 1 "Power")
            (footprint "R_0603"
                (uuid "footprint-uuid")
                (path "{old_kiid_path}")
                (property "Path" "Power.R1")
            )
            (group "Power"
                (uuid "group-uuid")
            )
        )"#
        );
        let expected = input
            .replace(&old_kiid_path, &new_kiid_path)
            .replace("\"Power.R1\"", "\"Block:Power.R1\"")
            .replace("(group \"Power\"", "(group \"Block:Power\"");
        let truncated_paths = HashMap::from([
            ("Power".to_string(), "Block:Power".to_string()),
            ("Power.R1".to_string(), "Block:Power.R1".to_string()),
        ]);

        let restore = |source: &str| {
            let (patches, _) =
                compute_truncated_path_patches(&parse(source).unwrap(), &truncated_paths);
            let mut buf = Vec::new();
            patches.write_to(source, &mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        };

        // Only the path, its KIID path, and the group change; the net and UUIDs stay.
        assert_eq!(restore(&input), expected);
        assert_eq!(restore(&expected), expected);
    }

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
        let old_uuid = uuid_for_path("Power.R1");
        let new_uuid = uuid_for_path("Supply.R1");

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

    #[test]
    fn test_net_only_rename_kicad10_net_syntax() {
        let input = r#"(kicad_pcb
            (group "Power"
                (uuid "123")
            )
            (footprint "R_0603"
                (property "Path" "Power.R1")
                (pad "1" smd rect (net "Power"))
            )
            (segment
                (net "Power")
            )
            (zone
                (net "Power")
            )
        )"#;

        let board = parse(input).unwrap();

        let mut renames = HashMap::new();
        renames.insert("Power".to_string(), "Supply".to_string());

        let (patches, applied) = super::compute_net_renames_patches(&board, &renames);
        let mut buf = Vec::new();
        patches.write_to(input, &mut buf).unwrap();
        let result = String::from_utf8(buf).unwrap();

        assert!(result.contains("(net \"Supply\")"));
        assert!(result.contains("\"Power.R1\""));
        assert!(result.contains("(group \"Power\""));
        assert_eq!(applied.len(), 3);
    }
}
