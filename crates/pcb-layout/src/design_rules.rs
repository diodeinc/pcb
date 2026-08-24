//! Generate the project's `.kicad_dru` custom design rules.
//!
//! `pcb layout` maintains one marker-delimited block in the file and
//! leaves everything outside it alone — KiCad's Board Setup UI edits
//! this same file, so hand-written rules must survive regeneration.
//!
//! The block carries the ICT test-point pitch rule: footprints synced
//! with a non-empty `Ict` property carry the `ICT` component class (see
//! `build_footprint_component_class_patchset`), and the rule holds any
//! two of them 2 mm apart edge-to-edge — 3 mm center-to-center for the
//! Ø1 mm ICT pads, the pitch the interposer's pogo footprint courtyard
//! demands. On boards with no ICT test points the rule matches nothing.

use std::path::Path;

use anyhow::{Context, Result};

const BEGIN: &str = "# --- rules managed by pcb layout: begin (do not edit this block) ---";
const END: &str = "# --- rules managed by pcb layout: end ---";
// Pad-scoped: physical_clearance would otherwise also measure the
// footprints' silkscreen text against each other.
const RULES: &str = r#"(rule "ict-test-point-pitch"
	(constraint physical_clearance (min 2.0mm))
	(condition "A.Type == 'Pad' && B.Type == 'Pad' && A.hasComponentClass('ICT') && B.hasComponentClass('ICT')"))"#;

/// Write or refresh the managed rules block in `dru_path`.
pub fn write_design_rules(dru_path: &Path) -> Result<()> {
    let block = format!("{BEGIN}\n{RULES}\n{END}");
    let current = std::fs::read_to_string(dru_path).unwrap_or_default();
    let next = match (current.find(BEGIN), current.find(END)) {
        (Some(begin), Some(end)) if end >= begin => {
            let mut merged = current.clone();
            merged.replace_range(begin..end + END.len(), &block);
            merged
        }
        _ if current.trim().is_empty() => format!("(version 1)\n\n{block}\n"),
        _ => format!("{}\n\n{block}\n", current.trim_end()),
    };
    if next != current {
        std::fs::write(dru_path, next).with_context(|| format!("write {}", dru_path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_merges_and_stays_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.kicad_dru");

        // Fresh file: version header plus the managed block.
        write_design_rules(&path).unwrap();
        let fresh = std::fs::read_to_string(&path).unwrap();
        assert!(fresh.starts_with("(version 1)\n"));
        assert!(fresh.contains("ict-test-point-pitch"));

        // Idempotent.
        write_design_rules(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), fresh);

        // User rules outside the block survive; a stale block is replaced.
        let user = fresh.replace("2.0mm", "9.9mm")
            + "(rule \"user-rule\"\n\t(constraint track_width (min 0.15mm)))\n";
        std::fs::write(&path, &user).unwrap();
        write_design_rules(&path).unwrap();
        let merged = std::fs::read_to_string(&path).unwrap();
        assert!(merged.contains("user-rule"));
        assert!(merged.contains("2.0mm"));
        assert!(!merged.contains("9.9mm"));
        assert_eq!(merged.matches("ict-test-point-pitch").count(), 1);
    }
}
