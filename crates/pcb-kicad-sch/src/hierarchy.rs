use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
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
    existing_module_pages: BTreeMap<String, BTreeSet<usize>>,
    linked_modules: Vec<LinkedModule>,
}

impl HierarchyPlan {
    pub(crate) fn root_page(&self) -> usize {
        self.root_page
    }

    pub fn page_for_new_component(&self, component_path: &str) -> Result<usize> {
        if let Some(sheet) = self
            .sheets
            .iter()
            .rev()
            .find(|sheet| is_descendant(component_path, &sheet.module_path))
        {
            return Ok(sheet.child_page);
        }

        let Some(module) = self
            .linked_modules
            .iter()
            .rev()
            .find(|module| is_descendant(component_path, &module.path))
        else {
            return Ok(self.root_page);
        };

        required_module_page(&self.existing_module_pages, &module.path)
    }
}

pub(crate) fn plan(
    mut linked_modules: Vec<LinkedModule>,
    existing_component_pages: BTreeMap<String, BTreeSet<usize>>,
    existing_page_ids: &[String],
    root_page: usize,
    first_new_page: usize,
) -> Result<HierarchyPlan> {
    linked_modules.sort_by(|left, right| {
        path_depth(&left.path)
            .cmp(&path_depth(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });

    // Assign each existing component to its closest linked module. A component
    // inside a nested linked child is evidence for the child page, not for the
    // page that owns the parent module's direct contents.
    let mut existing_module_pages = existing_component_pages
        .iter()
        .filter_map(|(component_path, pages)| {
            owning_module(component_path, &linked_modules)
                .map(|module| (module.path.clone(), pages))
        })
        .fold(
            BTreeMap::<String, BTreeSet<usize>>::new(),
            |mut modules, (module, pages)| {
                modules.entry(module).or_default().extend(pages);
                modules
            },
        );
    // A previously generated module page keeps its deterministic id even when
    // the user has emptied it of managed symbols; the page itself is the
    // authoritative evidence that the module is already materialized.
    for module in &linked_modules {
        if let Some(index) = existing_page_ids
            .iter()
            .position(|id| *id == page_id(&module.path))
        {
            existing_module_pages
                .entry(module.path.clone())
                .or_default()
                .insert(index);
        }
    }

    let mut sheets = Vec::<PlannedSheet>::new();
    for module in &linked_modules {
        let already_materialized = existing_module_pages.contains_key(&module.path)
            || existing_component_pages
                .keys()
                .any(|component_path| is_descendant(component_path, &module.path));
        if already_materialized {
            continue;
        }

        let parent_module = linked_modules
            .iter()
            .filter(|candidate| {
                candidate.path != module.path && is_descendant(&module.path, &candidate.path)
            })
            .max_by_key(|candidate| path_depth(&candidate.path));
        let parent_page = match parent_module {
            Some(parent) => match sheets.iter().find(|sheet| sheet.module_path == parent.path) {
                Some(sheet) => sheet.child_page,
                None => required_module_page(&existing_module_pages, &parent.path)?,
            },
            None => root_page,
        };
        let child_page = first_new_page + sheets.len();
        sheets.push(PlannedSheet {
            module_path: module.path.clone(),
            instance_ref: module.instance_ref.clone(),
            parent_page,
            child_page,
            file_name: format!("{}.kicad_sch", module.path),
        });
    }

    Ok(HierarchyPlan {
        sheets,
        root_page,
        existing_module_pages,
        linked_modules,
    })
}

fn unique_module_page(
    module_pages: &BTreeMap<String, BTreeSet<usize>>,
    module_path: &str,
) -> Result<Option<usize>> {
    let Some(pages) = module_pages.get(module_path) else {
        return Ok(None);
    };
    let mut pages = pages.iter().copied();
    let page = pages.next();
    if pages.next().is_some() {
        bail!(
            "linked module '{module_path}' has managed symbols on multiple schematic pages; cannot choose a page for new content"
        );
    }
    Ok(page)
}

fn required_module_page(
    module_pages: &BTreeMap<String, BTreeSet<usize>>,
    module_path: &str,
) -> Result<usize> {
    unique_module_page(module_pages, module_path)?.with_context(|| {
        format!(
            "linked module '{module_path}' has no page identified by a directly owned managed symbol; cannot choose a page for new content"
        )
    })
}

fn owning_module<'a>(
    component_path: &str,
    linked_modules: &'a [LinkedModule],
) -> Option<&'a LinkedModule> {
    linked_modules
        .iter()
        .filter(|module| is_descendant(component_path, &module.path))
        .max_by_key(|module| path_depth(&module.path))
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

    fn component_pages(entries: &[(&str, usize)]) -> BTreeMap<String, BTreeSet<usize>> {
        entries.iter().fold(
            BTreeMap::<String, BTreeSet<usize>>::new(),
            |mut pages, (path, page)| {
                pages.entry((*path).to_string()).or_default().insert(*page);
                pages
            },
        )
    }

    #[test]
    fn plans_one_page_per_uninitialized_instance() {
        let plan = plan(
            vec![module("POWER_B"), module("POWER_A")],
            BTreeMap::new(),
            &[],
            0,
            1,
        )
        .unwrap();

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
        assert_eq!(plan.page_for_new_component("POWER_A.R1").unwrap(), 1);
        assert_eq!(plan.page_for_new_component("POWER_B.R1").unwrap(), 2);
    }

    #[test]
    fn preserves_initialized_structure_and_nests_only_new_pages() {
        let plan = plan(
            vec![module("A"), module("A.NEW"), module("EXISTING")],
            component_pages(&[("A.R1", 4), ("EXISTING.R1", 7)]),
            &[],
            0,
            8,
        )
        .unwrap();

        assert_eq!(plan.sheets.len(), 1);
        assert_eq!(plan.sheets[0].module_path, "A.NEW");
        assert_eq!(plan.sheets[0].parent_page, 4);
        assert_eq!(plan.sheets[0].child_page, 8);
        assert_eq!(plan.page_for_new_component("EXISTING.R2").unwrap(), 7);
    }

    #[test]
    fn nested_components_do_not_select_the_parent_module_page() {
        let plan = plan(
            vec![module("A"), module("A.CHILD"), module("A.NEW")],
            component_pages(&[("A.CHILD.R1", 4), ("A.R1", 7)]),
            &[],
            0,
            8,
        )
        .unwrap();

        assert_eq!(plan.sheets.len(), 1);
        assert_eq!(plan.sheets[0].module_path, "A.NEW");
        assert_eq!(plan.sheets[0].parent_page, 7);
        assert_eq!(plan.page_for_new_component("A.R2").unwrap(), 7);
        assert_eq!(plan.page_for_new_component("A.CHILD.R2").unwrap(), 4);
    }

    #[test]
    fn stale_component_still_marks_its_module_initialized() {
        let plan = plan(
            vec![module("A")],
            component_pages(&[("A.REMOVED", 3)]),
            &[],
            0,
            4,
        )
        .unwrap();

        assert!(plan.sheets.is_empty());
        assert_eq!(plan.page_for_new_component("A.REPLACEMENT").unwrap(), 3);
    }

    #[test]
    fn rejects_missing_or_ambiguous_module_page_for_new_content() {
        let ambiguous = plan(
            vec![module("A")],
            component_pages(&[("A.R1", 2), ("A.R2", 3)]),
            &[],
            0,
            4,
        )
        .unwrap();

        assert_eq!(
            ambiguous
                .page_for_new_component("A.R3")
                .unwrap_err()
                .to_string(),
            "linked module 'A' has managed symbols on multiple schematic pages; cannot choose a page for new content"
        );

        let missing = plan(
            vec![module("A"), module("A.CHILD")],
            component_pages(&[("A.CHILD.R1", 2)]),
            &[],
            0,
            3,
        )
        .unwrap();
        assert_eq!(
            missing
                .page_for_new_component("A.R1")
                .unwrap_err()
                .to_string(),
            "linked module 'A' has no page identified by a directly owned managed symbol; cannot choose a page for new content"
        );
    }
}
