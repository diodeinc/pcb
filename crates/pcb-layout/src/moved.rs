//! Rename layout metadata without changing footprint or pad UUIDs.

use anyhow::{Result, ensure};
use pcb_sch::kicad_identity::{footprint_kiid_path, uuid_for_path};
use pcb_sch::{InstanceKind, Schematic};
use pcb_sexpr::board::{
    extract_keyed_footprints, is_footprint_kiid_path, is_footprint_path_property, is_group_name,
    is_net_name, is_zone_net_name,
};
use pcb_sexpr::{PatchSet, Sexpr, WalkCtx};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub(crate) fn compute_truncated_path_patches(
    board: &Sexpr,
    schematic: &Schematic,
) -> Result<(PatchSet, Vec<(String, String)>)> {
    let is_identity = |ctx: &WalkCtx<'_>| is_footprint_path_property(ctx) || is_group_name(ctx);
    let mut existing = HashSet::new();
    let mut footprint_paths = Vec::new();
    board.walk_strings(|value, _, ctx| {
        if is_identity(&ctx) {
            existing.insert(value.to_string());
        }
        if is_footprint_path_property(&ctx) {
            footprint_paths.push(value.to_string());
        }
    });

    let mut candidates: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (instance_ref, instance) in &schematic.instances {
        if !matches!(
            instance.kind,
            InstanceKind::Component | InstanceKind::Module
        ) {
            continue;
        }
        let path = instance_ref.instance_path.join(".");
        let truncated = path.rsplit(':').next().unwrap_or(&path).to_string();
        candidates.entry(truncated).or_default().insert(path);
    }

    let mut renames = HashMap::new();
    for (old, paths) in candidates {
        if !existing.contains(&old) || paths.iter().all(|path| existing.contains(path)) {
            continue;
        }
        ensure!(
            paths.len() == 1,
            "Cannot repair truncated layout path `{old}`: it matches multiple source instances: {}. Use moved() to select the intended instance before syncing.",
            paths.into_iter().collect::<Vec<_>>().join(", ")
        );
        renames.insert(old, paths.into_iter().next().unwrap());
    }

    let mut seen = HashSet::new();
    for fp in extract_keyed_footprints(board).map_err(anyhow::Error::msg)? {
        if let Some(path) = fp
            .properties
            .get("Path")
            .filter(|path| renames.contains_key(*path))
        {
            ensure!(
                fp.path == footprint_kiid_path(path) && seen.insert(path.clone()),
                "Cannot repair truncated layout path `{path}`: its footprint identity is inconsistent or duplicated"
            );
        }
    }
    for path in footprint_paths
        .iter()
        .filter(|path| renames.contains_key(*path))
    {
        ensure!(
            seen.contains(path),
            "Cannot repair truncated layout path `{path}`: its footprint has no schematic link"
        );
    }

    Ok(compute_path_patches(
        board,
        |path| renames.get(path).cloned(),
        is_identity,
    ))
}

/// Rename net references by exact match.
pub fn compute_net_renames_patches(
    board: &Sexpr,
    net_renames: &HashMap<String, String>,
) -> (PatchSet, Vec<(String, String)>) {
    if net_renames.is_empty() {
        return (PatchSet::default(), Vec::new());
    }
    compute_path_patches(
        board,
        |path| net_renames.get(path).cloned(),
        |ctx| is_net_name(ctx) || is_zone_net_name(ctx),
    )
}

/// Apply moved() using the longest matching path prefix.
pub fn compute_moved_paths_patches(
    board: &Sexpr,
    moved_paths: &HashMap<String, String>,
) -> (PatchSet, Vec<(String, String)>) {
    if moved_paths.is_empty() {
        return (PatchSet::default(), Vec::new());
    }
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

fn compute_path_patches(
    board: &Sexpr,
    rename: impl Fn(&str) -> Option<String>,
    is_patchable: impl Fn(&WalkCtx<'_>) -> bool,
) -> (PatchSet, Vec<(String, String)>) {
    let mut patches = PatchSet::default();
    let mut renames = Vec::new();

    let mut existing: HashSet<String> = HashSet::new();
    board.walk_strings(|value, _span, ctx| {
        if is_patchable(&ctx) {
            existing.insert(value.to_string());
        }
    });

    let mut link_renames = HashMap::new();
    board.walk_strings(|value, span, ctx| {
        if is_patchable(&ctx)
            && let Some(new_value) = rename(value)
            && !existing.contains(&new_value)
        {
            patches.replace_string(span, &new_value);
            if is_footprint_path_property(&ctx) {
                link_renames.insert(uuid_for_path(value), footprint_kiid_path(&new_value));
            }
            renames.push((value.to_string(), new_value));
        }
    });

    if !link_renames.is_empty() {
        board.walk_strings(|value, span, ctx| {
            if is_footprint_kiid_path(&ctx) {
                let trimmed = value.trim_start_matches('/');
                let first_uuid = trimmed.split('/').next().unwrap_or(trimmed);
                if let Some(new_link) = link_renames.get(first_uuid) {
                    patches.replace_string(span, new_link);
                }
            }
        });
    }

    (patches, renames)
}

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
    fn truncated_paths_reject_ambiguous_or_broken_identity() -> Result<()> {
        let mut schematic = Schematic::new();
        let module = ModuleRef::new("board.zen", "<root>");
        let add = |schematic: &mut Schematic, path: &str| {
            schematic.add_instance(
                InstanceRef::new(module.clone(), path.split('.').map(Into::into).collect()),
                Instance::component(module.clone()),
            );
        };
        add(&mut schematic, "Block:Power.R1");
        let board = format!(
            r#"(kicad_pcb (footprint "R" (property "Path" "Power.R1") (path "{}")))"#,
            footprint_kiid_path("Power.R1")
        );
        assert!(compute_truncated_path_patches(&parse(&board)?, &schematic).is_ok());
        let broken = board.replace(&footprint_kiid_path("Power.R1"), "/unmanaged");
        assert!(compute_truncated_path_patches(&parse(&broken)?, &schematic).is_err());
        let unlinked = r#"(kicad_pcb (footprint "R" (property "Path" "Power.R1")))"#;
        assert!(compute_truncated_path_patches(&parse(unlinked)?, &schematic).is_err());
        add(&mut schematic, "Other:Power.R1");
        let err = compute_truncated_path_patches(&parse(&board)?, &schematic).unwrap_err();
        assert!(err.to_string().contains("multiple source instances"));
        Ok(())
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
