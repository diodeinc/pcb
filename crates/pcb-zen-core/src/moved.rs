use std::collections::{HashMap, HashSet};

/// Longest-prefix remapper for old -> new path mapping.
/// Thread-safe after construction.
#[derive(Debug, Clone)]
pub struct Remapper {
    pub moved_paths: HashMap<String, String>, // old -> new
}

impl Remapper {
    pub fn from_path_map(path_map: HashMap<String, String>) -> Self {
        Self {
            moved_paths: path_map,
        }
    }

    #[cfg(test)]
    fn from_pairs(pairs: &[(String, String)]) -> Self {
        let mut map = HashMap::new();
        for (old, new) in pairs {
            if let Some(prev) = map.insert(old.clone(), new.clone()) {
                eprintln!(
                    "Warning: duplicate moved() directive old={} prev={} new={}",
                    old, prev, new
                );
            }
        }
        Self { moved_paths: map }
    }

    /// Remap a path using longest-prefix matching
    pub fn remap(&self, path: &str) -> Option<String> {
        let mut search_path = path;

        loop {
            if let Some(new_prefix) = self.moved_paths.get(search_path) {
                // path = prefix + remainder, where remainder is "" or ".foo.bar"
                let remainder = &path[search_path.len()..];
                return Some(format!("{}{}", new_prefix, remainder));
            }

            // Find previous dot; stop if none
            match search_path.rfind('.') {
                Some(pos) => search_path = &search_path[..pos],
                None => break,
            }
        }

        None
    }
}

/// Apply module scoping to a path
pub fn scoped_path(module_path: &str, local_path: &str) -> String {
    if module_path.is_empty() {
        local_path.to_string()
    } else {
        format!("{}.{}", module_path, local_path)
    }
}

/// Collect all existing paths from schematic instances and nets
pub fn collect_existing_paths(
    instances: &HashMap<pcb_sch::InstanceRef, pcb_sch::Instance>,
    nets: &HashMap<String, pcb_sch::Net>,
) -> HashSet<String> {
    let mut paths = HashSet::new();

    for instance_ref in instances.keys() {
        let path = instance_ref.instance_path.join(".");
        if !path.is_empty() {
            let parts: Vec<&str> = path.split('.').collect();
            for i in 1..=parts.len() {
                paths.insert(parts[0..i].join("."));
            }
        }
    }

    paths.extend(nets.keys().cloned());
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_moved_directive_storage() {
        use crate::lang::eval::EvalContext;
        use crate::lang::input::InputMap;
        use starlark::values::ValueLike;

        let test_content = r#"
moved("old.path.component", "new.path.component")
moved("POW.PS1", "PS1")
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
                moved_directives.get("old.path.component").unwrap().0,
                "new.path.component"
            );
            assert_eq!(moved_directives.get("POW.PS1").unwrap().0, "PS1");
            assert_eq!(
                moved_directives.get("Power.Reg1").unwrap().0,
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

        // Check the result
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

    // Remapper tests
    #[test]
    fn test_remapper_exact_matches() {
        let pairs = vec![
            ("A".to_string(), "B".to_string()),
            ("Power.Old".to_string(), "Power.New".to_string()),
        ];
        let remapper = Remapper::from_pairs(&pairs);

        // Forward mapping
        assert_eq!(remapper.remap("A"), Some("B".to_string()));
        assert_eq!(remapper.remap("Power.Old"), Some("Power.New".to_string()));
        assert_eq!(remapper.remap("Unknown"), None);
    }

    #[test]
    fn test_remapper_longest_prefix_matching() {
        let pairs = vec![
            ("Power".to_string(), "PWR".to_string()),
            ("Power.Supply".to_string(), "PWR.PS".to_string()), // More specific
            ("Net.Old".to_string(), "Net.New".to_string()),
        ];
        let remapper = Remapper::from_pairs(&pairs);

        // More specific match should win
        assert_eq!(remapper.remap("Power.Supply"), Some("PWR.PS".to_string()));
        assert_eq!(
            remapper.remap("Power.Supply.Component"),
            Some("PWR.PS.Component".to_string())
        );

        // Less specific match for other cases
        assert_eq!(remapper.remap("Power.Other"), Some("PWR.Other".to_string()));
        assert_eq!(
            remapper.remap("Power.Other.Deep.Path"),
            Some("PWR.Other.Deep.Path".to_string())
        );
    }

    #[test]
    fn test_remapper_boundary_conditions() {
        let pairs = vec![("POW.PS1".to_string(), "PS1".to_string())];
        let remapper = Remapper::from_pairs(&pairs);

        // Exact match
        assert_eq!(remapper.remap("POW.PS1"), Some("PS1".to_string()));

        // Children should match
        assert_eq!(
            remapper.remap("POW.PS1.component"),
            Some("PS1.component".to_string())
        );
        assert_eq!(
            remapper.remap("POW.PS1.deep.nested.path"),
            Some("PS1.deep.nested.path".to_string())
        );

        // Partial matches should NOT work
        assert_eq!(remapper.remap("POW.PS10"), None); // POW.PS10 != POW.PS1.*
        assert_eq!(remapper.remap("POW.PS"), None); // POW.PS != POW.PS1.*
    }

    #[test]
    fn test_remapper_overlapping_prefixes() {
        let pairs = vec![
            ("A".to_string(), "X".to_string()),
            ("A.B".to_string(), "Y.B".to_string()),
            ("A.B.C".to_string(), "Z.C".to_string()),
        ];
        let remapper = Remapper::from_pairs(&pairs);

        // Longest prefix should win
        assert_eq!(remapper.remap("A.B.C"), Some("Z.C".to_string()));
        assert_eq!(remapper.remap("A.B.C.D"), Some("Z.C.D".to_string()));

        assert_eq!(remapper.remap("A.B"), Some("Y.B".to_string()));
        assert_eq!(remapper.remap("A.B.X"), Some("Y.B.X".to_string()));

        assert_eq!(remapper.remap("A"), Some("X".to_string()));
        assert_eq!(remapper.remap("A.X"), Some("X.X".to_string()));
    }

    #[test]
    fn test_remapper_empty() {
        let remapper = Remapper::from_pairs(&[]);

        assert_eq!(remapper.remap("anything"), None);
    }

    #[test]
    fn test_remapper_net_symbols() {
        let pairs = vec![(
            "AN_OLD_FILTERED_VCC_VCC".to_string(),
            "FILTERED_VCC_VCC".to_string(),
        )];
        let remapper = Remapper::from_pairs(&pairs);

        // Net base name
        assert_eq!(
            remapper.remap("AN_OLD_FILTERED_VCC_VCC"),
            Some("FILTERED_VCC_VCC".to_string())
        );

        // Net with symbol suffix (for position comments)
        assert_eq!(
            remapper.remap("AN_OLD_FILTERED_VCC_VCC.1"),
            Some("FILTERED_VCC_VCC.1".to_string())
        );
        assert_eq!(
            remapper.remap("AN_OLD_FILTERED_VCC_VCC.2"),
            Some("FILTERED_VCC_VCC.2".to_string())
        );
    }

    #[test]
    fn test_schematic_moved_paths_integration() {
        use crate::convert::ToSchematic;
        use crate::lang::eval::EvalContext;
        use crate::lang::input::InputMap;

        let test_content = r#"
moved("old.component", "new.component")
moved("POW.PS1", "PS1")

# Simple content to make a valid module
"#;

        // Create a temporary file
        let temp_path = std::env::temp_dir().join("test_schematic_moved.zen");
        std::fs::write(&temp_path, test_content).unwrap();

        // Evaluate it
        let result = EvalContext::new()
            .set_source_path(temp_path.clone())
            .set_module_name("test_schematic_moved".to_string())
            .set_inputs(InputMap::new())
            .eval();

        // Clean up the temp file
        std::fs::remove_file(&temp_path).ok();

        if let Some(output) = result.output {
            // Convert to schematic using the diagnostics-aware method
            let schematic_result = output.sch_module.to_schematic_with_diagnostics();
            let schematic = schematic_result.output.unwrap();

            // Should get warnings about new paths not existing, and directives should be filtered out
            assert_eq!(schematic_result.diagnostics.len(), 2); // Two warnings about missing new paths

            // Check that moved_paths were filtered out due to warnings
            assert_eq!(schematic.moved_paths.len(), 0);
        }
    }
}
