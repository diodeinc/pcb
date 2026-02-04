use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn build_hierarchy_plan(ir: &ImportIr) -> ImportHierarchyPlan {
    let mut net_owner: BTreeMap<KiCadNetName, KiCadSheetPath> = BTreeMap::new();
    let mut modules: BTreeMap<KiCadSheetPath, ImportModuleBoundaryNets> = BTreeMap::new();

    for (sheet_path, node) in &ir.schematic_sheet_tree.nodes {
        modules.insert(
            sheet_path.clone(),
            ImportModuleBoundaryNets {
                sheet_name: node.sheet_name.clone(),
                nets_defined_here: BTreeSet::new(),
                nets_io_here: BTreeSet::new(),
            },
        );
    }

    for (net_name, net) in &ir.nets {
        let used_sheet_paths: BTreeSet<KiCadSheetPath> = net
            .ports
            .iter()
            .map(|p| KiCadSheetPath::from_sheetpath_tstamps(&p.component.sheetpath_tstamps))
            .collect();

        let owner = lca_sheet_paths(used_sheet_paths.iter());
        net_owner.insert(net_name.clone(), owner.clone());

        if let Some(owner_mod) = modules.get_mut(&owner) {
            owner_mod.nets_defined_here.insert(net_name.clone());
        }

        // For every module whose owner is an ancestor, and that contains at least one port in its
        // subtree, the net crosses the module boundary and must be declared as io() in that module.
        for (module_path, module) in modules.iter_mut() {
            if module_path == &owner {
                continue;
            }
            if !owner.is_ancestor_of(module_path) {
                continue;
            }
            let used_in_subtree = used_sheet_paths
                .iter()
                .any(|used| module_path.is_ancestor_of(used));
            if used_in_subtree {
                module.nets_io_here.insert(net_name.clone());
            }
        }
    }

    ImportHierarchyPlan { net_owner, modules }
}

fn lca_sheet_paths<'a, I>(mut paths: I) -> KiCadSheetPath
where
    I: Iterator<Item = &'a KiCadSheetPath>,
{
    let Some(first) = paths.next() else {
        return KiCadSheetPath::root();
    };
    let mut prefix: Vec<&str> = first.segments().collect();

    for path in paths {
        let segs: Vec<&str> = path.segments().collect();
        let mut new_len = 0usize;
        for (a, b) in prefix.iter().zip(segs.iter()) {
            if a == b {
                new_len += 1;
            } else {
                break;
            }
        }
        prefix.truncate(new_len);
        if prefix.is_empty() {
            break;
        }
    }

    if prefix.is_empty() {
        KiCadSheetPath::root()
    } else {
        KiCadSheetPath::from_sheetpath_tstamps(&format!("/{}/", prefix.join("/")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lca_works() {
        let a = KiCadSheetPath::from_sheetpath_tstamps("/a/");
        let b = KiCadSheetPath::from_sheetpath_tstamps("/a/b/");
        let c = KiCadSheetPath::from_sheetpath_tstamps("/a/c/");
        let r = KiCadSheetPath::root();

        assert_eq!(lca_sheet_paths([&a, &b].into_iter()).as_str(), "/a/");
        assert_eq!(lca_sheet_paths([&b, &c].into_iter()).as_str(), "/a/");
        assert_eq!(lca_sheet_paths([&b, &r].into_iter()).as_str(), "/");
    }
}
