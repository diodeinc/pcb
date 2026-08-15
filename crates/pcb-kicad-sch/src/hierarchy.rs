use std::collections::BTreeMap;

use pcb_sch::InstanceRef;

use crate::deterministic_uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LinkedModule {
    pub path: String,
    pub instance_ref: InstanceRef,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannedSheet {
    pub module_path: String,
    pub instance_ref: InstanceRef,
    pub parent_page: usize,
    pub child_page: usize,
    pub file_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HierarchyPlan {
    pub sheets: Vec<PlannedSheet>,
    root_page: usize,
    existing_component_pages: BTreeMap<String, usize>,
    linked_modules: Vec<LinkedModule>,
}

impl HierarchyPlan {
    pub fn page_for_new_component(&self, component_path: &str) -> usize {
        if let Some(sheet) = self
            .sheets
            .iter()
            .rev()
            .find(|sheet| is_descendant(component_path, &sheet.module_path))
        {
            return sheet.child_page;
        }

        self.linked_modules
            .iter()
            .rev()
            .find(|module| is_descendant(component_path, &module.path))
            .and_then(|module| self.representative_page(&module.path))
            .unwrap_or(self.root_page)
    }

    fn representative_page(&self, module_path: &str) -> Option<usize> {
        self.existing_component_pages
            .iter()
            .find(|(component_path, _)| is_descendant(component_path, module_path))
            .map(|(_, page)| *page)
    }
}

pub(crate) fn plan(
    mut linked_modules: Vec<LinkedModule>,
    existing_component_pages: BTreeMap<String, usize>,
    root_page: usize,
    first_new_page: usize,
) -> HierarchyPlan {
    linked_modules.sort_by(|left, right| {
        path_depth(&left.path)
            .cmp(&path_depth(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });

    let mut sheets = Vec::<PlannedSheet>::new();
    for module in &linked_modules {
        let has_managed_descendant = existing_component_pages
            .keys()
            .any(|component_path| is_descendant(component_path, &module.path));
        if has_managed_descendant {
            continue;
        }

        let parent_module = linked_modules
            .iter()
            .filter(|candidate| {
                candidate.path != module.path && is_descendant(&module.path, &candidate.path)
            })
            .max_by_key(|candidate| path_depth(&candidate.path));
        let parent_page = parent_module
            .and_then(|parent| {
                sheets
                    .iter()
                    .find(|sheet| sheet.module_path == parent.path)
                    .map(|sheet| sheet.child_page)
                    .or_else(|| {
                        existing_component_pages
                            .iter()
                            .find(|(path, _)| is_descendant(path, &parent.path))
                            .map(|(_, page)| *page)
                    })
            })
            .unwrap_or(root_page);
        let child_page = first_new_page + sheets.len();
        sheets.push(PlannedSheet {
            module_path: module.path.clone(),
            instance_ref: module.instance_ref.clone(),
            parent_page,
            child_page,
            file_name: format!("{}.kicad_sch", module.path),
        });
    }

    HierarchyPlan {
        sheets,
        root_page,
        existing_component_pages,
        linked_modules,
    }
}

pub(crate) fn page_id(module_path: &str) -> String {
    deterministic_uuid(format!("zener:module-page:{module_path}"))
}

pub(crate) fn sheet_id(module_path: &str) -> String {
    deterministic_uuid(format!("zener:module-sheet:{module_path}"))
}

fn is_descendant(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn path_depth(path: &str) -> usize {
    path.split('.').count()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use pcb_sch::{InstanceRef, ModuleRef};

    use super::*;

    fn module(path: &str) -> LinkedModule {
        LinkedModule {
            path: path.to_string(),
            instance_ref: InstanceRef::new(
                ModuleRef::from_path(Path::new("/test.zen"), path),
                path.split('.').map(Into::into).collect(),
            ),
        }
    }

    #[test]
    fn plans_one_page_per_uninitialized_instance() {
        let plan = plan(
            vec![module("POWER_B"), module("POWER_A")],
            BTreeMap::new(),
            0,
            1,
        );

        assert_eq!(
            plan.sheets
                .iter()
                .map(|sheet| (&sheet.module_path, sheet.parent_page, sheet.child_page))
                .collect::<Vec<_>>(),
            vec![
                (&"POWER_A".to_string(), 0, 1),
                (&"POWER_B".to_string(), 0, 2)
            ]
        );
        assert_eq!(plan.page_for_new_component("POWER_A.R1"), 1);
        assert_eq!(plan.page_for_new_component("POWER_B.R1"), 2);
    }

    #[test]
    fn preserves_initialized_structure_and_nests_only_new_pages() {
        let plan = plan(
            vec![module("A"), module("A.NEW"), module("EXISTING")],
            BTreeMap::from([("A.R1".to_string(), 4), ("EXISTING.R1".to_string(), 7)]),
            0,
            8,
        );

        assert_eq!(plan.sheets.len(), 1);
        assert_eq!(plan.sheets[0].module_path, "A.NEW");
        assert_eq!(plan.sheets[0].parent_page, 4);
        assert_eq!(plan.sheets[0].child_page, 8);
        assert_eq!(plan.page_for_new_component("EXISTING.R2"), 7);
    }
}
