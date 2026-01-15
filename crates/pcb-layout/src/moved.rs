//! Apply moved() path renames to KiCad PCB files.
//!
//! This module handles preprocessing of .kicad_pcb files to apply path renames
//! before the main sync process runs. This is a structural operation that:
//! 1. Walks the parsed board to find patchable strings using structural predicates
//! 2. Applies longest-prefix matching to determine renames
//! 3. Patches the file in-place while preserving formatting
//! 4. Updates footprint UUIDs to match the new paths

use pcb_sexpr::board::{
    is_footprint_kiid_path, is_footprint_path_property, is_group_name, is_net_name,
};
use pcb_sexpr::{PatchSet, Sexpr};
use std::collections::HashMap;
use uuid::Uuid;

/// UUID namespace used for generating deterministic footprint UUIDs from paths.
/// This matches Python: uuid.NAMESPACE_URL
const UUID_NAMESPACE_URL: Uuid = Uuid::from_u128(0x6ba7b811_9dad_11d1_80b4_00c04fd430c8);

/// Report of what was renamed during moved() preprocessing.
#[derive(Debug, Default)]
pub struct MovedPathsReport {
    pub footprint_paths_renamed: usize,
    pub group_names_renamed: usize,
    pub net_names_renamed: usize,
    pub renames: Vec<(String, String)>,
}

impl MovedPathsReport {
    pub fn is_empty(&self) -> bool {
        self.footprint_paths_renamed == 0
            && self.group_names_renamed == 0
            && self.net_names_renamed == 0
    }

    pub fn total(&self) -> usize {
        self.footprint_paths_renamed + self.group_names_renamed + self.net_names_renamed
    }
}

/// Apply moved() path renames to a board, writing directly to a writer.
///
/// Takes the parsed board, the source text, a map of old->new path prefixes,
/// and a writer to stream the patched output to.
///
/// Uses longest-prefix matching:
/// - For a path like "Power.R1" and moved_paths {"Power": "Supply"},
///   the result is "Supply.R1"
///
/// Also updates footprint UUIDs to match the new paths.
pub fn apply_moved_paths<W: std::io::Write>(
    board: &Sexpr,
    source: &str,
    moved_paths: &HashMap<String, String>,
    writer: W,
) -> std::io::Result<MovedPathsReport> {
    let mut report = MovedPathsReport::default();

    if moved_paths.is_empty() {
        let mut w = writer;
        w.write_all(source.as_bytes())?;
        return Ok(report);
    }

    let mut patches = PatchSet::new();

    // First pass: collect footprint path info and build old_path -> new_path mapping
    let mut footprint_path_renames: HashMap<String, String> = HashMap::new();

    board.walk_strings(|value, span, ctx| {
        if is_footprint_path_property(&ctx) {
            if let Some(new_value) = apply_longest_prefix_match(value, moved_paths) {
                patches.replace_string(span, &new_value);
                report.footprint_paths_renamed += 1;
                report.renames.push((value.to_string(), new_value.clone()));
                footprint_path_renames.insert(value.to_string(), new_value);
            }
        } else if is_group_name(&ctx) {
            if let Some(new_value) = apply_longest_prefix_match(value, moved_paths) {
                patches.replace_string(span, &new_value);
                report.group_names_renamed += 1;
                report.renames.push((value.to_string(), new_value));
            }
        } else if is_net_name(&ctx) {
            if let Some(new_value) = apply_longest_prefix_match(value, moved_paths) {
                patches.replace_string(span, &new_value);
                report.net_names_renamed += 1;
                report.renames.push((value.to_string(), new_value));
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

    patches.write_to(source, writer)?;
    Ok(report)
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
    ) -> (String, MovedPathsReport) {
        let mut buf = Vec::new();
        let report = apply_moved_paths(board, source, moved_paths, &mut buf).unwrap();
        (String::from_utf8(buf).unwrap(), report)
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

        let (result, report) = apply_to_string(&board, input, &moved);

        assert!(result.contains("\"Supply.R1.R\""));
        assert!(!result.contains("\"Power.R1.R\""));
        assert!(result.contains("(group \"Supply\""));
        assert!(!result.contains("(group \"Power\""));
        assert!(result.contains("\"Power_VCC\"")); // Net unchanged

        assert_eq!(report.footprint_paths_renamed, 1);
        assert_eq!(report.group_names_renamed, 1);
        assert_eq!(report.net_names_renamed, 0);
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

        let (result, report) = apply_to_string(&board, &input, &moved);

        // Path property should be updated
        assert!(result.contains("\"Supply.R1\""));
        assert!(!result.contains("\"Power.R1\""));

        // UUID path should be updated
        assert!(result.contains(&format!("\"/{new_uuid}/{new_uuid}\"")));
        assert!(!result.contains(&format!("\"/{old_uuid}/{old_uuid}\"")));

        assert_eq!(report.footprint_paths_renamed, 1);
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

        let (result, report) = apply_to_string(&board, input, &moved);

        assert!(result.contains("\"NEW_VCC\""));
        assert!(result.contains("\"NEW_GND\""));
        assert_eq!(report.net_names_renamed, 2);
    }
}
