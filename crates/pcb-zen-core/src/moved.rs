use crate::lang::module::FrozenModuleValue;
use pcb_sch::InstanceRef;
use std::collections::HashMap;

/// Apply moved directives to remap a path
///
/// This simplified implementation supports exact path matching and module hierarchy remapping:
/// - If the old path matches exactly, it gets remapped to the new path
/// - If the old path is a prefix (module), all child paths are automatically remapped
pub fn apply_moved_directives(
    path: &str,
    moved_directives: &HashMap<String, String>,
) -> Option<String> {
    // Try exact match first
    if let Some(new_path) = moved_directives.get(path) {
        return Some(new_path.clone());
    }

    // Try prefix matches (module hierarchy remapping)
    for (old_path, new_path) in moved_directives.iter() {
        // Check if path starts with old_path as a module prefix
        if path.starts_with(old_path) {
            let remaining = &path[old_path.len()..];
            // Ensure we're matching at module boundaries (either end of string or followed by '.')
            if remaining.is_empty() {
                // Exact match - already handled above
                continue;
            } else if remaining.starts_with('.') {
                // Module hierarchy match: old_path.something -> new_path.something
                return Some(format!("{}{}", new_path, remaining));
            }
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
    fn test_module_hierarchy_remapping() {
        let mut directives = HashMap::new();
        // Move module POW.PS1 to PS1 - this should handle all children automatically
        directives.insert("POW.PS1".to_string(), "PS1".to_string());
        directives.insert("Power".to_string(), "PWR".to_string());

        // Direct module match
        assert_eq!(
            apply_moved_directives("POW.PS1", &directives),
            Some("PS1".to_string())
        );

        // Module hierarchy - children get remapped automatically
        assert_eq!(
            apply_moved_directives("POW.PS1.some_resistor", &directives),
            Some("PS1.some_resistor".to_string())
        );

        assert_eq!(
            apply_moved_directives("POW.PS1.inner.component", &directives),
            Some("PS1.inner.component".to_string())
        );

        // Different module
        assert_eq!(
            apply_moved_directives("Power.Regulator", &directives),
            Some("PWR.Regulator".to_string())
        );

        // Partial matches shouldn't work (POW.PS10 shouldn't match POW.PS1)
        assert_eq!(apply_moved_directives("POW.PS10", &directives), None);
    }

    #[test]
    fn test_net_remapping() {
        let mut directives = HashMap::new();
        directives.insert(
            "AN_OLD_FILTERED_VCC_VCC".to_string(),
            "FILTERED_VCC_VCC".to_string(),
        );

        // Direct net name remapping
        assert_eq!(
            apply_moved_directives("AN_OLD_FILTERED_VCC_VCC", &directives),
            Some("FILTERED_VCC_VCC".to_string())
        );

        // Net symbol remapping (for position comments)
        assert_eq!(
            apply_moved_directives("AN_OLD_FILTERED_VCC_VCC.1", &directives),
            Some("FILTERED_VCC_VCC.1".to_string())
        );
    }

    #[test]
    fn test_component_moves() {
        let mut directives = HashMap::new();
        directives.insert(
            "ModuleA.ComponentX".to_string(),
            "ModuleB.ComponentX".to_string(),
        );

        assert_eq!(
            apply_moved_directives("ModuleA.ComponentX", &directives),
            Some("ModuleB.ComponentX".to_string())
        );

        // Properties should also get remapped
        assert_eq!(
            apply_moved_directives("ModuleA.ComponentX.pin1", &directives),
            Some("ModuleB.ComponentX.pin1".to_string())
        );
    }

    #[test]
    fn test_no_false_matches() {
        let mut directives = HashMap::new();
        directives.insert("Power".to_string(), "PWR".to_string());

        // PowerSupply shouldn't match "Power" prefix
        assert_eq!(apply_moved_directives("PowerSupply", &directives), None);

        // Power.Main should match
        assert_eq!(
            apply_moved_directives("Power.Main", &directives),
            Some("PWR.Main".to_string())
        );
    }

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
                moved_directives.get("old.path.component").unwrap(),
                "new.path.component"
            );
            assert_eq!(moved_directives.get("POW.PS1").unwrap(), "PS1");
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
}
