use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use pcb_sch::InstanceRef;

use crate::{
    SchDocument, SchItem, SchPage, SymbolInstance, SymbolSlotKey,
    connectivity::kicad::{page_instances, resolve_file_name},
    deterministic_uuid,
    kicad::find_child,
    model::SheetInstance,
};

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

    pub(crate) fn page_for_new_component(&self, component_path: &str) -> Result<usize> {
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

/// Remove obsolete generated pages, without imposing generated organization on
/// live content. Both the filename and UUID must still identify a module page;
/// renamed/user-created pages and references remain authoritative.
pub(crate) fn prune_obsolete_pages(
    document: &mut SchDocument,
    modules: &[LinkedModule],
) -> Result<()> {
    let module_paths = document
        .pages
        .iter()
        .map(|page| {
            let path = std::path::Path::new(page.file_name.as_deref()?)
                .file_stem()?
                .to_str()?;
            (page.id == page_id(path)).then(|| path.to_string())
        })
        .collect::<Vec<_>>();
    let by_file = document
        .pages
        .iter()
        .enumerate()
        .filter_map(|(index, page)| {
            page.file_name
                .as_deref()
                .and_then(|name| crate::sheet_file("", name).ok())
                .map(|name| (name, index))
        })
        .collect::<BTreeMap<_, _>>();
    let mut retained = BTreeSet::new();
    for (index, page) in document.pages.iter().enumerate() {
        let mut keep = module_paths[index]
            .as_ref()
            .is_none_or(|path| modules.iter().any(|module| &module.path == path))
            || document.root_page_ids.contains(&page.id);
        for item in &page.items {
            keep |= match item {
                SchItem::Graphic(_) => true,
                // Opaque drawings (images, Beziers) have item UUIDs, but
                // electrical buses are also parsed as unsupported items.
                SchItem::Unsupported(node) => {
                    !matches!(
                        node.as_list().and_then(|items| items.first()?.as_sym()),
                        Some("bus" | "bus_entry")
                    ) && node.find_list("uuid").is_some()
                }
                SchItem::Symbol(symbol) => {
                    symbol.field_value("Path").is_some()
                        || page
                            .library
                            .definitions
                            .get(symbol.library_key())
                            .map(crate::symbol::ParsedSymbolDefinition::parse)
                            .transpose()?
                            .is_some_and(|definition| definition.is_unmanaged_graphic(symbol))
                }
                _ => false,
            };
        }
        if keep {
            retained.insert(index);
        }
    }
    let mut relationships = Vec::new();
    for (parent, page) in document.pages.iter().enumerate() {
        for sheet in page.items.iter().filter_map(|item| match item {
            SchItem::Sheet(sheet) => Some(sheet),
            _ => None,
        }) {
            let Some(&child) = resolve_file_name(page, sheet.file_name())
                .ok()
                .and_then(|file| by_file.get(&file))
            else {
                continue;
            };
            if module_paths[child].as_ref().is_none_or(|path| {
                sheet.id != sheet_id(path) || sheet.file_name() != format!("{path}.kicad_sch")
            }) {
                retained.insert(child);
            }
            relationships.push((parent, child, sheet.id.clone()));
        }
    }
    // A live or user-organized descendant keeps every ancestor, including
    // shared pages. Iterate to a fixed point so page order does not matter.
    loop {
        let previous = retained.len();
        for (parent, child, _) in &relationships {
            if retained.contains(child) {
                retained.insert(*parent);
            }
        }
        if retained.len() == previous {
            break;
        }
    }
    for (parent, child, sheet_id) in relationships {
        if !retained.contains(&child) {
            document.pages[parent]
                .items
                .retain(|item| !matches!(item, SchItem::Sheet(sheet) if sheet.id == sheet_id));
        }
    }
    let mut index = 0;
    document.pages.retain(|_| {
        let keep = retained.contains(&index);
        index += 1;
        keep
    });
    Ok(())
}

pub(crate) fn is_descendant(path: &str, ancestor: &str) -> bool {
    path == ancestor
        || path
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn path_depth(path: &str) -> usize {
    path.split('.').count()
}

/// Fill in symbol annotations and sheet page numbers per sheet-instance path
/// the way KiCad does on load: unnumbered sheets take the lowest unused page
/// numbers in sheet-list order, and paths under this project's roots belong
/// to this project even after a rename.
pub(crate) fn sync_instances(
    document: &mut SchDocument,
    slots: &BTreeSet<SymbolSlotKey>,
) -> Result<()> {
    let project = document.project_name.clone();
    let instances = page_instances(document)?;
    let page_of = instances
        .iter()
        .map(|instance| (instance.id.as_str(), instance.page))
        .collect::<BTreeMap<_, _>>();
    let mut paths_by_page = BTreeMap::<String, Vec<String>>::new();
    let mut used = BTreeSet::new();
    let mut unnumbered = Vec::new();
    // Sheets KiCad cannot reach (unplaced, or below an unplaced sheet) take no page number.
    let mut unreachable = BTreeSet::new();
    for instance in &instances {
        paths_by_page
            .entry(instance.page.id.clone())
            .or_default()
            .push(format!("/{}", instance.id));
        // Roots are numbered by `sheet_instances`, child sheets by their sheet item.
        let (page, sheet) = match instance.id.rsplit_once('/') {
            None => (root_page_number(instance.page), None),
            Some((parent_id, sheet_id)) => {
                let parent = page_of[parent_id];
                let sheet = parent
                    .items
                    .iter()
                    .find_map(|item| match item {
                        SchItem::Sheet(sheet) if sheet.id == sheet_id => Some(sheet),
                        _ => None,
                    })
                    .context("sheet instance without a sheet item")?;
                if !sheet.placed || unreachable.contains(parent_id) {
                    unreachable.insert(instance.id.as_str());
                    continue;
                }
                let parent_path = format!("/{parent_id}");
                let page = sheet
                    .instances
                    .iter()
                    .find(|instance| instance.path == parent_path)
                    .map(|instance| instance.page.clone());
                let sheet = (parent.id.clone(), sheet_id.to_string(), parent_path);
                (page, Some(sheet))
            }
        };
        match page {
            Some(page) => {
                used.insert(page);
            }
            None => unnumbered.push(sheet),
        }
    }
    drop(instances);
    let mut new_pages = BTreeMap::<(String, String), Vec<SheetInstance>>::new();
    let free = (1..).map(|n| n.to_string()).filter(|n| !used.contains(n));
    for (sheet, page) in unnumbered.into_iter().zip(free) {
        // An unnumbered root still takes the first number.
        if let Some((page_id, sheet_id, path)) = sheet {
            new_pages
                .entry((page_id, sheet_id))
                .or_default()
                .push(SheetInstance {
                    project: project.to_string(),
                    path,
                    page,
                });
        }
    }

    let SchDocument {
        pages,
        root_page_ids,
        ..
    } = document;
    let is_root_path = |path: &str| {
        let root = path
            .strip_prefix('/')
            .and_then(|path| path.split('/').next());
        !project.is_empty() && root.is_some_and(|root| root_page_ids.iter().any(|id| id == root))
    };
    for page in pages {
        let Some(paths) = paths_by_page.get(&page.id) else {
            continue;
        };
        for item in &mut page.items {
            match item {
                SchItem::Symbol(symbol) => {
                    let slot = symbol
                        .field_value("Path")
                        .and_then(|path| SymbolSlotKey::new(path, symbol.unit))
                        .filter(|slot| slots.contains(slot));
                    let reference = match (symbol.reference(), &slot) {
                        (Some(reference), _) => reference.to_string(),
                        (None, Some(slot)) => {
                            bail!("managed symbol '{slot}' has no Reference field")
                        }
                        (None, None) => continue,
                    };
                    for path in paths {
                        match symbol.instances.iter_mut().find(|i| &i.path == path) {
                            // Only managed annotations follow the Reference field.
                            Some(instance) => {
                                if slot.is_some() {
                                    instance.reference = Some(reference.clone());
                                    instance.unit = Some(symbol.unit);
                                }
                            }
                            None => symbol.instances.push(SymbolInstance {
                                project: project.to_string(),
                                path: path.clone(),
                                reference: Some(reference.clone()),
                                unit: Some(symbol.unit),
                                unsupported: Vec::new(),
                            }),
                        }
                    }
                    for instance in &mut symbol.instances {
                        if is_root_path(&instance.path) {
                            instance.project = project.to_string();
                        }
                    }
                    symbol.instances.sort_by(|a, b| a.path.cmp(&b.path));
                }
                SchItem::Sheet(sheet) => {
                    if let Some(added) = new_pages.remove(&(page.id.clone(), sheet.id.clone())) {
                        sheet.instances.extend(added);
                    }
                    for instance in &mut sheet.instances {
                        if is_root_path(&instance.path) {
                            instance.project = project.to_string();
                        }
                    }
                    sheet.instances.sort_by(|a, b| a.path.cmp(&b.path));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// The root page number from `(sheet_instances (path "/" (page "N")))`.
fn root_page_number(page: &SchPage) -> Option<String> {
    page.items.iter().find_map(|item| {
        let SchItem::Unsupported(sexpr) = item else {
            return None;
        };
        let path = find_child(
            find_child(std::slice::from_ref(sexpr), "sheet_instances")?,
            "path",
        )?;
        (path.get(1)?.as_atom()? == "/").then_some(())?;
        find_child(path, "page")?
            .get(1)?
            .as_atom()
            .map(str::to_string)
    })
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

    fn page(path: &str, children: &[&str]) -> crate::SchPage {
        let mut page = crate::SchPage::new(page_id(path));
        page.file_name = Some(format!("eda/{path}.kicad_sch"));
        page.items = children
            .iter()
            .map(|child| {
                SchItem::Sheet(Box::new(crate::Sheet {
                    instances: Vec::new(),
                    id: sheet_id(child),
                    placed: true,
                    at: None,
                    size: None,
                    name: None,
                    file: crate::SymbolField::new(
                        "Sheetfile",
                        format!("{child}.kicad_sch"),
                        crate::Point::new(0.0, 0.0),
                    ),
                    pins: Vec::new(),
                    unsupported: Vec::new(),
                }))
            })
            .collect();
        page
    }

    #[test]
    fn prunes_obsolete_subtrees_but_preserves_live_descendants_and_user_pages() {
        let mut notes = page("NOTES", &[]);
        notes.id = "user-page".to_string();
        let mut document = SchDocument {
            root_page_ids: vec![page_id("ROOT")],
            // Deliberately put leaves first: retention must propagate more
            // than one level and must not depend on load order.
            pages: vec![
                page("OLD.CHILD", &[]),
                page("KEEP.MIDDLE.CHILD", &[]),
                page("ROOT", &["OLD", "KEEP", "NOTES"]),
                page("KEEP", &["KEEP.MIDDLE"]),
                page("KEEP.MIDDLE", &["KEEP.MIDDLE.CHILD"]),
                page("OLD", &["OLD.CHILD"]),
                notes.clone(),
            ],
            ..Default::default()
        };
        prune_obsolete_pages(&mut document, &[module("KEEP.MIDDLE.CHILD")]).unwrap();
        assert_eq!(
            document
                .pages
                .iter()
                .map(|page| page.id.clone())
                .collect::<Vec<_>>(),
            ["KEEP.MIDDLE.CHILD", "ROOT", "KEEP", "KEEP.MIDDLE"]
                .map(page_id)
                .into_iter()
                .chain([notes.id.clone()])
                .collect::<Vec<_>>()
        );
        assert_eq!(document.pages.last(), Some(&notes));
        assert_eq!(
            document.pages[1].items,
            page("ROOT", &["KEEP", "NOTES"]).items
        );
    }

    #[test]
    fn renamed_or_user_referenced_module_pages_are_not_pruned() {
        for organization in ["user-reference", "renamed", "relocated"] {
            let mut child = page("OLD", &[]);
            let mut root = page("ROOT", &["OLD"]);
            let SchItem::Sheet(sheet) = &mut root.items[0] else {
                unreachable!()
            };
            match organization {
                "renamed" => {
                    child.file_name = Some("eda/renamed.kicad_sch".to_string());
                    sheet.file.value = "renamed.kicad_sch".to_string();
                }
                "relocated" => {
                    child.file_name = Some("eda/archive/OLD.kicad_sch".to_string());
                    sheet.file.value = "archive/OLD.kicad_sch".to_string();
                }
                _ => sheet.id = "user-reference".to_string(),
            }
            let mut document = SchDocument {
                root_page_ids: vec![root.id.clone()],
                pages: vec![root, child],
                ..Default::default()
            };
            let original = document.clone();
            prune_obsolete_pages(&mut document, &[]).unwrap();
            assert_eq!(document, original);
        }
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
