use crate::lang::module::FrozenModuleValue;
use pcb_sch::InstanceRef;
use std::collections::HashMap;

/// Token for segment-aware glob patterns
#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// Literal segment that must match exactly
    Lit(String),
    /// Single wildcard (*) that matches exactly one segment
    Star,
    /// Deep wildcard (**) that matches zero or more segments
    DeepStar,
}

/// Compiled moved directive rule for efficient matching
#[derive(Debug, Clone)]
struct MovedRule {
    /// Source pattern tokens
    src: Vec<Token>,
    /// Destination pattern tokens  
    dst: Vec<Token>,
    /// Number of leading literal tokens (for prioritization)
    lit_prefix_len: usize,
    /// Original source pattern string
    #[allow(unused)]
    src_pattern: String,
    /// Original destination pattern string  
    #[allow(unused)]
    dst_pattern: String,
}

impl MovedRule {
    /// Compile a moved directive pattern pair into a rule
    fn compile(src_pattern: &str, dst_pattern: &str) -> Result<Self, String> {
        let src = Self::parse_pattern(src_pattern)?;
        let dst = Self::parse_pattern(dst_pattern)?;

        // Validate that wildcards match between src and dst
        let src_wildcards: Vec<_> = src
            .iter()
            .filter(|t| matches!(t, Token::Star | Token::DeepStar))
            .collect();
        let dst_wildcards: Vec<_> = dst
            .iter()
            .filter(|t| matches!(t, Token::Star | Token::DeepStar))
            .collect();

        if src_wildcards.len() != dst_wildcards.len() {
            return Err(format!(
                "Wildcard count mismatch: source has {}, destination has {}",
                src_wildcards.len(),
                dst_wildcards.len()
            ));
        }

        for (src_wild, dst_wild) in src_wildcards.iter().zip(dst_wildcards.iter()) {
            if src_wild != dst_wild {
                return Err("Wildcard type mismatch".to_string());
            }
        }

        let lit_prefix_len = src
            .iter()
            .take_while(|t| matches!(t, Token::Lit(_)))
            .count();

        Ok(MovedRule {
            src,
            dst,
            lit_prefix_len,
            src_pattern: src_pattern.to_string(),
            dst_pattern: dst_pattern.to_string(),
        })
    }

    /// Parse a pattern string into tokens
    fn parse_pattern(pattern: &str) -> Result<Vec<Token>, String> {
        if pattern.is_empty() {
            return Ok(vec![]);
        }

        let mut tokens = Vec::new();
        for segment in pattern.split('.') {
            match segment {
                "*" => tokens.push(Token::Star),
                "**" => tokens.push(Token::DeepStar),
                lit => {
                    if lit.contains('*') {
                        return Err("Wildcards must be complete segments".to_string());
                    }
                    tokens.push(Token::Lit(lit.to_string()));
                }
            }
        }
        Ok(tokens)
    }

    /// Try to match this rule against a path and return the transformed path
    fn try_match(&self, segments: &[String]) -> Option<String> {
        let captures = self.match_segments(&self.src, segments)?;
        Some(self.substitute_captures(&self.dst, &captures))
    }

    /// Match pattern against segments, returning captured values
    fn match_segments(&self, pattern: &[Token], segments: &[String]) -> Option<Vec<String>> {
        let mut captures = Vec::new();
        let mut seg_idx = 0;
        let mut pat_idx = 0;

        while pat_idx < pattern.len() {
            if seg_idx >= segments.len() {
                return None; // Pattern longer than segments
            }

            match &pattern[pat_idx] {
                Token::Lit(lit) => {
                    if segments[seg_idx] != *lit {
                        return None;
                    }
                    seg_idx += 1;
                    pat_idx += 1;
                }
                Token::Star => {
                    captures.push(segments[seg_idx].clone());
                    seg_idx += 1;
                    pat_idx += 1;
                }
                Token::DeepStar => {
                    // ** matches zero or more segments
                    if pat_idx + 1 == pattern.len() {
                        // ** at the end matches all remaining segments
                        let remaining = if seg_idx < segments.len() {
                            segments[seg_idx..].join(".")
                        } else {
                            String::new() // Empty for zero segments
                        };
                        captures.push(remaining);
                        return Some(captures);
                    } else {
                        // Find how many segments to consume for **
                        let remaining_pattern = &pattern[pat_idx + 1..];
                        let remaining_segments = &segments[seg_idx..];
                        if let Some(consumed) =
                            self.find_deepstar_match(remaining_pattern, remaining_segments)
                        {
                            let captured = if consumed > 0 {
                                segments[seg_idx..seg_idx + consumed].join(".")
                            } else {
                                String::new()
                            };
                            captures.push(captured);
                            seg_idx += consumed;
                            pat_idx += 1;
                        } else {
                            return None;
                        }
                    }
                }
            }
        }

        // Must consume all segments
        if seg_idx == segments.len() {
            Some(captures)
        } else {
            None
        }
    }

    /// Helper for matching ** wildcards
    fn find_deepstar_match(
        &self,
        remaining_pattern: &[Token],
        remaining_segments: &[String],
    ) -> Option<usize> {
        // Try consuming 0 to all remaining segments
        for consumed in 0..=remaining_segments.len() {
            let rest = &remaining_segments[consumed..];
            if self.match_segments(remaining_pattern, rest).is_some() {
                return Some(consumed);
            }
        }
        None
    }

    /// Substitute captures into destination pattern
    fn substitute_captures(&self, pattern: &[Token], captures: &[String]) -> String {
        let mut result = Vec::new();
        let mut capture_idx = 0;

        for token in pattern {
            match token {
                Token::Lit(lit) => result.push(lit.clone()),
                Token::Star | Token::DeepStar => {
                    if capture_idx < captures.len() {
                        let capture = &captures[capture_idx];
                        if !capture.is_empty() {
                            result.push(capture.clone());
                        }
                        capture_idx += 1;
                    }
                }
            }
        }

        result.join(".")
    }
}

/// Apply moved directives to remap a path
pub fn apply_moved_directives(
    path: &str,
    moved_directives: &HashMap<String, String>,
) -> Option<String> {
    // Compile rules and sort by specificity (most specific first)
    let mut rules = Vec::new();
    for (old_pattern, new_pattern) in moved_directives.iter() {
        if let Ok(rule) = MovedRule::compile(old_pattern, new_pattern) {
            rules.push(rule);
        }
    }

    // Sort by literal prefix length (most specific first)
    rules.sort_by(|a, b| b.lit_prefix_len.cmp(&a.lit_prefix_len));

    // Split path into segments
    let segments: Vec<String> = if path.is_empty() {
        vec![]
    } else {
        path.split('.').map(|s| s.to_string()).collect()
    };

    // Try each rule in order
    for rule in rules {
        if let Some(result) = rule.try_match(&segments) {
            return Some(result);
        }
    }

    None
}

/// Process position remapping with module scoping for all modules in a hierarchy
pub fn process_position_remapping(
    module_instances: &[(InstanceRef, FrozenModuleValue)],
) -> HashMap<String, String> {
    let mut all_moved_directives = HashMap::new();

    for (instance_ref, module) in module_instances {
        let module_path = instance_ref.instance_path.join(".");
        for (old_path, new_path) in module.moved_directives().iter() {
            // Apply module scope: prefix both old and new paths with module path
            let scoped_old_path = if module_path.is_empty() {
                old_path.clone()
            } else {
                format!("{}.{}", module_path, old_path)
            };
            let scoped_new_path = if module_path.is_empty() {
                new_path.clone()
            } else {
                format!("{}.{}", module_path, new_path)
            };
            all_moved_directives.insert(scoped_old_path, scoped_new_path);
        }
    }

    all_moved_directives
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_matching() {
        let mut directives = HashMap::new();
        directives.insert("OldComponent".to_string(), "NewComponent".to_string());
        directives.insert(
            "Power.OldReg.C1".to_string(),
            "PowerMgmt.VoltageReg.C1".to_string(),
        );

        assert_eq!(
            apply_moved_directives("OldComponent", &directives),
            Some("NewComponent".to_string())
        );

        assert_eq!(
            apply_moved_directives("Power.OldReg.C1", &directives),
            Some("PowerMgmt.VoltageReg.C1".to_string())
        );

        assert_eq!(
            apply_moved_directives("UnknownComponent", &directives),
            None
        );
    }

    #[test]
    fn test_single_wildcard() {
        let mut directives = HashMap::new();
        directives.insert(
            "Power.*.Regulator".to_string(),
            "Power.*.VoltageReg".to_string(),
        );

        assert_eq!(
            apply_moved_directives("Power.ADC.Regulator", &directives),
            Some("Power.ADC.VoltageReg".to_string())
        );

        assert_eq!(
            apply_moved_directives("Power.Main.Regulator", &directives),
            Some("Power.Main.VoltageReg".to_string())
        );

        // Doesn't match different structure
        assert_eq!(
            apply_moved_directives("Power.ADC.Filter", &directives),
            None
        );
    }

    #[test]
    fn test_deep_wildcard() {
        let mut directives = HashMap::new();
        directives.insert("Electrical.**".to_string(), "**".to_string()); // Remove Electrical prefix
        directives.insert("Power.**".to_string(), "PWR.**".to_string()); // Rename prefix

        assert_eq!(
            apply_moved_directives("Electrical.Analog.AMP.U1", &directives),
            Some("Analog.AMP.U1".to_string())
        );

        assert_eq!(
            apply_moved_directives("Power.Main.Regulator.C1", &directives),
            Some("PWR.Main.Regulator.C1".to_string())
        );
    }

    #[test]
    fn test_priority_order() {
        let mut directives = HashMap::new();
        // More specific rule should win
        directives.insert("Power.*".to_string(), "PWR.*".to_string());
        directives.insert("Power.Main".to_string(), "MainPower".to_string());

        // More specific rule (longer literal prefix) should win
        assert_eq!(
            apply_moved_directives("Power.Main", &directives),
            Some("MainPower".to_string())
        );

        // Less specific rule applies to other cases
        assert_eq!(
            apply_moved_directives("Power.Secondary", &directives),
            Some("PWR.Secondary".to_string())
        );
    }

    #[test]
    fn test_hierarchical_patterns() {
        let mut directives = HashMap::new();
        // Move entire hierarchies with **
        directives.insert("POW.PS1.**".to_string(), "PS1.**".to_string());
        directives.insert("Old.**".to_string(), "New.Prefix.**".to_string());

        assert_eq!(
            apply_moved_directives("POW.PS1.C_FILTER.C", &directives),
            Some("PS1.C_FILTER.C".to_string())
        );

        assert_eq!(
            apply_moved_directives("POW.PS1.R_PULLUP.R", &directives),
            Some("PS1.R_PULLUP.R".to_string())
        );

        // Test adding prefix with **
        assert_eq!(
            apply_moved_directives("Old.Component.Subcomponent", &directives),
            Some("New.Prefix.Component.Subcomponent".to_string())
        );

        // For exact match of POW.PS1, we need a separate rule
        directives.insert("POW.PS1".to_string(), "PS1".to_string());

        assert_eq!(
            apply_moved_directives("POW.PS1", &directives),
            Some("PS1".to_string())
        );
    }

    #[test]
    fn test_mixed_patterns() {
        let mut directives = HashMap::new();
        directives.insert("Power.*.Filter.*".to_string(), "PWR.*.Filt.*".to_string());
        directives.insert("Board.**.Component".to_string(), "PCB.**.Comp".to_string());

        assert_eq!(
            apply_moved_directives("Power.Main.Filter.C1", &directives),
            Some("PWR.Main.Filt.C1".to_string())
        );

        assert_eq!(
            apply_moved_directives("Board.Level1.Level2.Component", &directives),
            Some("PCB.Level1.Level2.Comp".to_string())
        );
    }

    #[test]
    fn test_real_world_scenarios() {
        let mut directives = HashMap::new();

        // Net name changes
        directives.insert(
            "AN_OLD_FILTERED_VCC_VCC.*".to_string(),
            "FILTERED_VCC_VCC.*".to_string(),
        );

        // Module reorganization
        directives.insert("POW.**".to_string(), "PowerMgmt.**".to_string());

        // Component renames
        directives.insert("*.OldRegulator.*".to_string(), "*.VoltageReg.*".to_string());

        // Test net symbol remapping
        assert_eq!(
            apply_moved_directives("AN_OLD_FILTERED_VCC_VCC.1", &directives),
            Some("FILTERED_VCC_VCC.1".to_string())
        );

        // Test module hierarchy moves
        assert_eq!(
            apply_moved_directives("POW.PS1.C_FILTER.C", &directives),
            Some("PowerMgmt.PS1.C_FILTER.C".to_string())
        );

        // Test component renames within any module
        assert_eq!(
            apply_moved_directives("Power.OldRegulator.Properties", &directives),
            Some("Power.VoltageReg.Properties".to_string())
        );
    }

    #[test]
    fn test_invalid_patterns() {
        // Wildcard count mismatch
        assert!(MovedRule::compile("Power.*", "PWR").is_err());

        // Wildcard type mismatch
        assert!(MovedRule::compile("Power.*", "PWR.**").is_err());

        // Invalid wildcard in segment
        assert!(MovedRule::compile("Power.R*", "PWR.R*").is_err());
    }

    #[test]
    fn test_moved_directive_storage() {
        use crate::lang::eval::EvalContext;
        use crate::lang::input::InputMap;
        use starlark::values::ValueLike;

        let test_content = r#"
moved("old.path.component", "new.path.component")
moved("foo.bar.*", "baz.bar.*")
moved("Power.Reg1", "PowerMgmt.Reg1")
"#;

        // Create a temporary file
        let temp_path = std::env::temp_dir().join("test_moved.zen");
        std::fs::write(&temp_path, test_content).unwrap();

        // Evaluate it
        let result = EvalContext::new()
            .set_source_path(temp_path.clone())
            .set_module_name("test_moved".to_string())
            .set_inputs(InputMap::new())
            .eval();

        // Clean up the temp file
        std::fs::remove_file(&temp_path).ok();

        // Check the result
        assert!(result.output.is_some(), "Evaluation should succeed");
        let output = result.output.unwrap();

        if let Some(frozen_ctx) = output
            .star_module
            .extra_value()
            .and_then(|extra| extra.downcast_ref::<crate::lang::context::FrozenContextValue>())
        {
            let moved_directives = frozen_ctx.module.moved_directives();
            assert_eq!(moved_directives.len(), 3, "Should have 3 moved directives");

            assert_eq!(
                moved_directives.get("old.path.component").unwrap(),
                "new.path.component"
            );
            assert_eq!(moved_directives.get("foo.bar.*").unwrap(), "baz.bar.*");
            assert_eq!(
                moved_directives.get("Power.Reg1").unwrap(),
                "PowerMgmt.Reg1"
            );
        } else {
            panic!("Could not access frozen context");
        }
    }

    #[test]
    fn test_moved_directive_empty() {
        use crate::lang::eval::EvalContext;
        use crate::lang::input::InputMap;
        use starlark::values::ValueLike;

        let test_content = r#"
# No moved directives
load("@stdlib/interfaces.zen", "Power")
vcc = Power("3V3")
"#;

        // Create a temporary file
        let temp_path = std::env::temp_dir().join("test_moved_empty.zen");
        std::fs::write(&temp_path, test_content).unwrap();

        // Evaluate it
        let result = EvalContext::new()
            .set_source_path(temp_path.clone())
            .set_module_name("test_moved_empty".to_string())
            .set_inputs(InputMap::new())
            .eval();

        // Clean up the temp file
        std::fs::remove_file(&temp_path).ok();

        // Check the result - skip if stdlib isn't available in test environment
        if let Some(output) = result.output {
            if let Some(frozen_ctx) = output
                .star_module
                .extra_value()
                .and_then(|extra| extra.downcast_ref::<crate::lang::context::FrozenContextValue>())
            {
                let moved_directives = frozen_ctx.module.moved_directives();
                assert_eq!(moved_directives.len(), 0, "Should have no moved directives");
            }
        }
    }
}
