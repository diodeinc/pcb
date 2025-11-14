use scan_fmt::scan_fmt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Seek, Write};
use std::path::Path;

use crate::natural_string::NaturalString;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub rotation: f64,
}

pub fn parse_position_comments(content: &str) -> (BTreeMap<NaturalString, Position>, usize) {
    let mut positions = BTreeMap::new();
    let mut block_start = content.len();

    // Walk backwards through lines, tracking byte position
    for line in content.rsplit_terminator('\n') {
        let line_start = block_start.saturating_sub(line.len() + 1); // +1 for '\n'

        match line.trim() {
            "" => {
                // Empty line - still in position block
                block_start = line_start;
            }
            trimmed if trimmed.starts_with("# pcb:sch ") => {
                // Position comment - parse it
                if let Ok((element_id, x, y, rotation)) = scan_fmt!(
                    trimmed,
                    "# pcb:sch {} x={} y={} rot={}",
                    String,
                    f64,
                    f64,
                    f64
                ) {
                    positions.insert(NaturalString::from(element_id), Position { x, y, rotation });
                } else {
                    log::warn!("Malformed pcb:sch comment: {}", line.trim());
                }
                block_start = line_start; // Extend block upward
            }
            _ => {
                // First non-position line - stop parsing
                break;
            }
        }
    }

    (positions, block_start)
}

pub fn update_position_comments(
    content: &str,
    new_positions: &BTreeMap<String, Position>,
) -> (usize, String) {
    // Get existing positions and block start
    let (mut existing_positions, block_start) = parse_position_comments(content);

    // Merge new positions (overriding existing ones)
    for (element_id, position) in new_positions {
        existing_positions.insert(NaturalString::from(element_id.clone()), position.clone());
    }

    // Check if we need a blank line before positions
    let content_before = &content[..block_start];
    let needs_blank_line = !content_before.is_empty() && !content_before.ends_with("\n\n");

    // Generate position comments
    let mut position_comments = String::new();
    if needs_blank_line {
        if content_before.ends_with('\n') {
            position_comments.push('\n'); // Add one more to create blank line
        } else {
            position_comments.push_str("\n\n"); // Add newline + blank line
        }
    }

    // BTreeMap with NaturalString keys automatically sorts naturally
    for (element_id, position) in &existing_positions {
        let comment = format!(
            "# pcb:sch {} x={:.4} y={:.4} rot={:.0}\n",
            element_id, position.x, position.y, position.rotation
        );
        position_comments.push_str(&comment);
    }

    (block_start, position_comments)
}

pub fn replace_pcb_sch_comments<P: AsRef<Path>>(
    file_path: P,
    positions: &BTreeMap<String, Position>,
) -> std::io::Result<()> {
    // Read existing content
    let content = std::fs::read_to_string(&file_path)?;

    // Get truncation position and new position comments
    let (truncate_pos, position_comments) = update_position_comments(&content, positions);

    // Open file for read+write (don't truncate the whole file)
    let mut file = OpenOptions::new().write(true).read(true).open(&file_path)?;

    // Truncate at position block start and append new comments
    file.set_len(truncate_pos as u64)?;
    file.seek(std::io::SeekFrom::Start(truncate_pos as u64))?;
    file.write_all(position_comments.as_bytes())?;
    file.flush()?;

    Ok(())
}

/// Convert a stable symbol ID (e.g. "comp:R1" or "sym:NET#2") to the
/// comment key used in `# pcb:sch` lines (e.g. "R1" or "NET.2").
pub fn symbol_id_to_comment_key(symbol_id: &str) -> Option<String> {
    if let Some(component_name) = symbol_id.strip_prefix("comp:") {
        Some(component_name.to_string())
    } else {
        symbol_id
            .strip_prefix("sym:")
            .map(|net_symbol| net_symbol.replace('#', "."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_position_comments() {
        let content = r#"
load("@stdlib/interfaces.zen", "Power")

# pcb:sch AD7171 x=241.3000 y=203.2000 rot=0
# pcb:sch C_BULK.C x=558.8000 y=88.9000 rot=0
# pcb:sch R_PULLUP.R x=723.9000 y=88.9000 rot=180
"#;

        let (positions, _block_start) = parse_position_comments(content);

        assert_eq!(positions.len(), 3);
        assert_eq!(positions["AD7171"].x, 241.3000);
        assert_eq!(positions["AD7171"].y, 203.2000);
        assert_eq!(positions["AD7171"].rotation, 0.0);

        assert_eq!(positions["R_PULLUP.R"].rotation, 180.0);
    }

    #[test]
    fn test_update_position_comments() {
        let original_content = r#"load("@stdlib/interfaces.zen", "Power")

# Old position comment
# pcb:sch OLD_ELEMENT x=100.0000 y=200.0000 rot=90"#;

        let mut positions = std::collections::BTreeMap::new();
        positions.insert(
            "NEW_ELEMENT".to_string(),
            Position {
                x: 300.0,
                y: 400.0,
                rotation: 45.0,
            },
        );

        let (truncate_pos, position_comments) =
            update_position_comments(original_content, &positions);
        let updated_content = format!("{}{}", &original_content[..truncate_pos], position_comments);

        // Old position comment should be preserved (merge behavior)
        assert!(updated_content.contains("OLD_ELEMENT"));

        // New position comment should be added
        assert!(updated_content.contains("# pcb:sch NEW_ELEMENT x=300.0000 y=400.0000 rot=45"));

        // Original content should be preserved
        assert!(updated_content.contains("load(\"@stdlib/interfaces.zen\""));
    }

    #[test]
    fn test_update_existing_positions_no_extra_blank_line() {
        let original_content = r#"load("@stdlib/interfaces.zen", "Power")

# pcb:sch EXISTING_ELEMENT x=100.0000 y=200.0000 rot=90"#;

        let mut positions = std::collections::BTreeMap::new();
        positions.insert(
            "NEW_ELEMENT".to_string(),
            Position {
                x: 300.0,
                y: 400.0,
                rotation: 45.0,
            },
        );

        let (truncate_pos, position_comments) =
            update_position_comments(original_content, &positions);
        let updated_content = format!("{}{}", &original_content[..truncate_pos], position_comments);

        // Should not add extra blank lines when updating existing position comments
        let blank_lines = updated_content.matches("\n\n").count();
        assert_eq!(blank_lines, 1); // Only one blank line after load statement

        // Should preserve existing position comment (merge behavior)
        assert!(updated_content.contains("EXISTING_ELEMENT"));
        assert!(updated_content.contains("NEW_ELEMENT"));
    }

    #[test]
    fn test_parse_element_ids_with_spaces_ignored() {
        let content = r#"
# pcb:sch CAN_TERM_SW.Can Termination Switch.JS202011SCQN.JS202011SCQN x=123.4 y=567.8 rot=90
# pcb:sch NORMAL_ELEMENT x=100.0 y=200.0 rot=0
# pcb:sch Another Element With Spaces x=300.0 y=400.0 rot=180
"#;

        let (positions, _block_start) = parse_position_comments(content);

        // Only elements without spaces should parse successfully
        assert_eq!(positions.len(), 1);
        assert_eq!(positions["NORMAL_ELEMENT"].x, 100.0);

        // Elements with spaces should be ignored (scan_fmt limitation)
        assert!(
            !positions.contains_key("CAN_TERM_SW.Can Termination Switch.JS202011SCQN.JS202011SCQN")
        );
        assert!(!positions.contains_key("Another Element With Spaces"));
    }

    #[test]
    fn test_malformed_lines_ignored_and_removed() {
        let original_content = r#"load("@stdlib/interfaces.zen", "Power")

# Valid position comment  
# pcb:sch VALID_ELEMENT x=100.0 y=200.0 rot=0

# Malformed position comments at end (backward parsing will find these)
# pcb:sch CAN_TERM_SW.Can Termination Switch.JS202011SCQN x=123.0 y=456.0 rot=90
# pcb:sch MISSING_ROTATION x=300.0 y=400.0
# pcb:sch INVALID_NUMBER x=not_a_number y=500.0 rot=0"#;

        // Test 1: Backward parsing should ignore malformed block at end
        let (final_block_positions, _block_start) = parse_position_comments(original_content);
        assert_eq!(final_block_positions.len(), 0); // No valid positions in malformed final block

        // Test 2: Full file scan should find valid positions anywhere
        let mut new_positions = std::collections::BTreeMap::new();
        new_positions.insert(
            "NEW_ELEMENT".to_string(),
            Position {
                x: 700.0,
                y: 800.0,
                rotation: 45.0,
            },
        );

        // Test 3: Update function should preserve valid positions and add new ones
        let (truncate_pos, position_comments) =
            update_position_comments(original_content, &new_positions);
        let updated_content = format!("{}{}", &original_content[..truncate_pos], position_comments);

        // Should preserve valid positions from anywhere in file
        assert!(updated_content.contains("VALID_ELEMENT")); // From early in file
        assert!(updated_content.contains("NEW_ELEMENT")); // Newly added

        // Should remove malformed position comments (they're truncated away)
        assert!(!updated_content.contains("CAN_TERM_SW.Can Termination"));
        assert!(!updated_content.contains("MISSING_ROTATION"));
        assert!(!updated_content.contains("INVALID_NUMBER"));

        // Should preserve original code
        assert!(updated_content.contains("load(\"@stdlib/interfaces.zen\""));

        // Should have exactly two pcb:sch lines (VALID_ELEMENT + NEW_ELEMENT)
        let pcb_sch_count = updated_content.matches("# pcb:sch ").count();
        assert_eq!(pcb_sch_count, 2);
    }

    #[test]
    fn test_merge_preserves_existing_positions() {
        let original_content = r#"load("@stdlib/interfaces.zen", "Power")

# pcb:sch EXISTING_A x=100.0 y=200.0 rot=90
# pcb:sch EXISTING_B x=300.0 y=400.0 rot=180"#;

        let mut new_positions = std::collections::BTreeMap::new();
        new_positions.insert(
            "EXISTING_A".to_string(),
            Position {
                x: 150.0,
                y: 250.0,
                rotation: 45.0,
            },
        ); // Override A
        new_positions.insert(
            "NEW_C".to_string(),
            Position {
                x: 500.0,
                y: 600.0,
                rotation: 270.0,
            },
        ); // Add C

        let (content_before, position_comments) =
            update_position_comments(original_content, &new_positions);
        let updated_content = format!("{content_before}{position_comments}");

        // Should have 3 elements: updated A, preserved B, new C
        let pcb_sch_count = updated_content.matches("# pcb:sch ").count();
        assert_eq!(pcb_sch_count, 3);

        // EXISTING_A should be overridden
        assert!(updated_content.contains("# pcb:sch EXISTING_A x=150.0000 y=250.0000 rot=45"));

        // EXISTING_B should be preserved
        assert!(updated_content.contains("# pcb:sch EXISTING_B x=300.0000 y=400.0000 rot=180"));

        // NEW_C should be added
        assert!(updated_content.contains("# pcb:sch NEW_C x=500.0000 y=600.0000 rot=270"));

        // Should be sorted alphabetically
        let positions_section = updated_content.split("\n\n").last().unwrap();
        let lines: Vec<&str> = positions_section.lines().collect();
        assert!(lines[0].contains("EXISTING_A"));
        assert!(lines[1].contains("EXISTING_B"));
        assert!(lines[2].contains("NEW_C"));
    }

    #[test]
    fn test_backward_parsing_stops_at_non_position() {
        let content = r#"load("@stdlib/interfaces.zen", "Power")

# This is a regular comment
# pcb:sch VALID_B x=300.0 y=400.0 rot=0
# pcb:sch VALID_C x=500.0 y=600.0 rot=0"#;

        let (positions, block_start) = parse_position_comments(content);

        // Should only parse the bottom 2 positions (stops at regular comment)
        assert_eq!(positions.len(), 2);
        assert!(positions.contains_key("VALID_B")); // In position block
        assert!(positions.contains_key("VALID_C")); // In position block

        // Block start should be at VALID_B line
        assert!(content[block_start..].contains("VALID_B"));
        assert!(!content[block_start..].contains("regular comment"));
    }

    #[test]
    fn test_interleaved_pcb_sch_comments() {
        // Test content with pcb:sch comments scattered throughout
        let content = r#"load("@stdlib/interfaces.zen", "Power")

# Early position comment (should be ignored by backward parsing)
# pcb:sch EARLY_ELEMENT x=100.0 y=200.0 rot=0

Resistor = Module("@stdlib/generics/Resistor.zen")
vcc = Power("VCC")
gnd = Ground("GND")

# Position comment in the middle (should be ignored)
# pcb:sch MIDDLE_ELEMENT x=300.0 y=400.0 rot=90

Resistor("R1", "10kOhm", "0603", P1=vcc.NET, P2=gnd.NET)

# Some final comment before positions
# This line should stop the backward parsing

# Final position block (only these should be parsed)
# pcb:sch FINAL_A x=500.0 y=600.0 rot=180  
# pcb:sch FINAL_B x=700.0 y=800.0 rot=270"#;

        let (positions, block_start) = parse_position_comments(content);

        // Should only parse the final position block (2 elements)
        assert_eq!(positions.len(), 2);
        assert!(!positions.contains_key("EARLY_ELEMENT")); // Above non-position content
        assert!(!positions.contains_key("MIDDLE_ELEMENT")); // Above non-position content
        assert!(positions.contains_key("FINAL_A")); // In final block
        assert!(positions.contains_key("FINAL_B")); // In final block

        // Block start should be at beginning of final block
        let content_from_block = &content[block_start..];
        assert!(content_from_block.contains("FINAL_A"));
        assert!(content_from_block.contains("FINAL_B"));
        assert!(!content_from_block.contains("This line should stop"));
        assert!(!content_from_block.contains("EARLY_ELEMENT"));
        assert!(!content_from_block.contains("MIDDLE_ELEMENT"));

        // Test that merge preserves the final block positions
        let mut new_positions = std::collections::BTreeMap::new();
        new_positions.insert(
            "FINAL_A".to_string(),
            Position {
                x: 999.0,
                y: 888.0,
                rotation: 45.0,
            },
        ); // Override
        new_positions.insert(
            "NEW_ELEMENT".to_string(),
            Position {
                x: 111.0,
                y: 222.0,
                rotation: 0.0,
            },
        ); // Add

        let (truncate_pos, position_comments) = update_position_comments(content, &new_positions);
        let updated_content = format!("{}{}", &content[..truncate_pos], position_comments);

        // Should preserve all scattered positions + new ones (5 total: EARLY, MIDDLE, FINAL_A, FINAL_B, NEW)
        assert_eq!(updated_content.matches("# pcb:sch ").count(), 5);
        assert!(updated_content.contains("# pcb:sch FINAL_A x=999.0000 y=888.0000 rot=45")); // Overridden
        assert!(updated_content.contains("# pcb:sch FINAL_B x=700.0000 y=800.0000 rot=270")); // Preserved
        assert!(updated_content.contains("# pcb:sch NEW_ELEMENT x=111.0000 y=222.0000 rot=0")); // Added

        // Should now contain the scattered positions (preserved in merge)
        assert!(updated_content.contains("EARLY_ELEMENT"));
        assert!(updated_content.contains("MIDDLE_ELEMENT"));

        // Should preserve all the original code
        assert!(updated_content.contains("load(\"@stdlib/interfaces.zen\""));
        assert!(updated_content.contains("Resistor(\"R1\""));
        assert!(updated_content.contains("This line should stop"));
    }

    #[test]
    fn test_empty_file() {
        let content = "";
        let (positions, block_start) = parse_position_comments(content);

        assert_eq!(positions.len(), 0);
        assert_eq!(block_start, 0);
    }

    #[test]
    fn test_file_with_only_positions() {
        let content = r#"# pcb:sch A x=100.0 y=200.0 rot=0
# pcb:sch B x=300.0 y=400.0 rot=90"#;

        let (positions, block_start) = parse_position_comments(content);

        assert_eq!(positions.len(), 2);
        assert_eq!(block_start, 0); // Block starts at beginning
        assert!(positions.contains_key("A"));
        assert!(positions.contains_key("B"));
    }

    #[test]
    fn test_file_with_no_positions() {
        let content = r#"load("@stdlib/interfaces.zen", "Power")

Resistor = Module("@stdlib/generics/Resistor.zen")"#;

        let (positions, block_start) = parse_position_comments(content);

        assert_eq!(positions.len(), 0);
        assert_eq!(block_start, content.len()); // Block start at end (no positions found)
    }

    #[test]
    fn test_negative_and_decimal_coordinates() {
        let content = r#"# pcb:sch NEG_COORDS x=-123.4567 y=-987.6543 rot=0
# pcb:sch DECIMAL_ROT x=100.0 y=200.0 rot=45.5"#;

        let (positions, _) = parse_position_comments(content);

        assert_eq!(positions.len(), 2);
        assert_eq!(positions["NEG_COORDS"].x, -123.4567);
        assert_eq!(positions["NEG_COORDS"].y, -987.6543);
        assert_eq!(positions["DECIMAL_ROT"].rotation, 45.5);
    }

    #[test]
    fn test_whitespace_variations() {
        let content = r#"   # pcb:sch INDENTED x=100.0 y=200.0 rot=0   
		# pcb:sch TABS x=300.0 y=400.0 rot=90
#pcb:sch NO_SPACE x=500.0 y=600.0 rot=180"#;

        let (positions, _) = parse_position_comments(content);

        // Backward parsing stops early due to malformed final line
        assert_eq!(positions.len(), 0); // NO_SPACE line is malformed and stops parsing
        assert!(!positions.contains_key("INDENTED")); // Above stopping point
        assert!(!positions.contains_key("TABS")); // Above stopping point
        assert!(!positions.contains_key("NO_SPACE")); // Malformed
    }

    #[test]
    fn test_file_ending_without_newline() {
        let content = "load(\"test\")\n# pcb:sch ELEMENT x=100.0 y=200.0 rot=0";

        let mut new_positions = std::collections::BTreeMap::new();
        new_positions.insert(
            "NEW".to_string(),
            Position {
                x: 300.0,
                y: 400.0,
                rotation: 90.0,
            },
        );

        let (truncate_pos, position_comments) = update_position_comments(content, &new_positions);
        let updated_content = format!("{}{}", &content[..truncate_pos], position_comments);

        // Should handle file without trailing newline
        assert!(updated_content.contains("load(\"test\")"));
        assert!(updated_content.contains("NEW"));
        assert!(updated_content.contains("ELEMENT")); // Preserved from merge
    }

    #[test]
    fn test_only_whitespace_at_end() {
        let content = r#"load("@stdlib/interfaces.zen", "Power")

# pcb:sch ELEMENT x=100.0 y=200.0 rot=0



"#;

        let (positions, block_start) = parse_position_comments(content);

        assert_eq!(positions.len(), 1);
        assert!(positions.contains_key("ELEMENT"));

        // Block should include the position comment
        let content_from_block = &content[block_start..];
        assert!(content_from_block.contains("# pcb:sch ELEMENT"));
    }

    #[test]
    fn test_replace_pcb_sch_comments_file_operations() {
        use std::fs;
        use tempfile::NamedTempFile;

        // Create temporary file
        let temp_file = NamedTempFile::new().expect("Failed to create temp file");
        let temp_path = temp_file.path();

        // Write initial content
        let initial_content = r#"load("@stdlib/interfaces.zen", "Power")

# pcb:sch OLD_ELEMENT x=100.0 y=200.0 rot=0"#;
        fs::write(temp_path, initial_content).expect("Failed to write initial content");

        // Update positions
        let mut new_positions = std::collections::BTreeMap::new();
        new_positions.insert(
            "NEW_ELEMENT".to_string(),
            Position {
                x: 300.0,
                y: 400.0,
                rotation: 90.0,
            },
        );

        // Test file update
        replace_pcb_sch_comments(temp_path, &new_positions).expect("Failed to replace comments");

        // Verify updated content
        let updated_content = fs::read_to_string(temp_path).expect("Failed to read updated file");
        assert!(updated_content.contains("load(\"@stdlib/interfaces.zen\""));
        assert!(updated_content.contains("NEW_ELEMENT"));
        assert!(updated_content.contains("OLD_ELEMENT")); // Should be preserved by merge
    }

    #[test]
    fn test_multiple_blank_lines_and_mixed_whitespace() {
        let content = "load(\"test\")\n\n\n# pcb:sch A x=1.0 y=2.0 rot=0\n\n# pcb:sch B x=3.0 y=4.0 rot=90\n\n\n";

        let (positions, block_start) = parse_position_comments(content);

        assert_eq!(positions.len(), 2);
        assert!(positions.contains_key("A"));
        assert!(positions.contains_key("B"));

        // Block should start at first position comment
        assert!(content[block_start..].contains("pcb:sch A"));
    }

    #[test]
    fn test_extremely_long_element_id() {
        let long_id = "A".repeat(1000);
        let content = format!("# pcb:sch {long_id} x=100.0 y=200.0 rot=0");

        let (positions, _) = parse_position_comments(&content);

        assert_eq!(positions.len(), 1);
        assert!(positions.contains_key(long_id.as_str()));
        assert_eq!(positions[long_id.as_str()].x, 100.0);
    }

    #[test]
    fn test_newline_insertion() {
        // Test file ending with code (no newline) - should add blank line before positions
        let content = r#"load("@stdlib/interfaces.zen", "Power")
Resistor("R1", "10kOhm", "0603", P1=vcc.NET, P2=gnd.NET)"#;

        let mut new_positions = std::collections::BTreeMap::new();
        new_positions.insert(
            "NEW_ELEMENT".to_string(),
            Position {
                x: 100.0,
                y: 200.0,
                rotation: 0.0,
            },
        );

        let (truncate_pos, position_comments) = update_position_comments(content, &new_positions);
        let updated_content = format!("{}{}", &content[..truncate_pos], position_comments);

        println!("Updated content: '{updated_content}'");

        // Should have blank line between code and position comments
        assert!(updated_content.contains("P2=gnd.NET)\n\n# pcb:sch NEW_ELEMENT"));
        assert!(!updated_content.contains("P2=gnd.NET)\n# pcb:sch NEW_ELEMENT"));
        // No missing blank line
    }

    #[test]
    fn test_natural_numeric_sorting() {
        let content = "";
        let mut positions = std::collections::BTreeMap::new();
        positions.insert(
            "v3v3_VCC.10".to_string(),
            Position {
                x: 100.0,
                y: 200.0,
                rotation: 0.0,
            },
        );
        positions.insert(
            "v3v3_VCC.2".to_string(),
            Position {
                x: 300.0,
                y: 400.0,
                rotation: 0.0,
            },
        );
        positions.insert(
            "v3v3_VCC.9".to_string(),
            Position {
                x: 500.0,
                y: 600.0,
                rotation: 0.0,
            },
        );
        positions.insert(
            "v3v3_VCC.11".to_string(),
            Position {
                x: 700.0,
                y: 800.0,
                rotation: 0.0,
            },
        );

        let (_, position_comments) = update_position_comments(content, &positions);
        println!("Position comments order:\n{position_comments}");

        // Should sort numerically: 2, 9, 10, 11
        let lines: Vec<&str> = position_comments
            .lines()
            .filter(|l| l.contains("pcb:sch"))
            .collect();
        assert!(lines[0].contains("v3v3_VCC.2"));
        assert!(lines[1].contains("v3v3_VCC.9"));
        assert!(lines[2].contains("v3v3_VCC.10"));
        assert!(lines[3].contains("v3v3_VCC.11"));
    }
}
