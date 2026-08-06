use std::{collections::BTreeSet, path::PathBuf};

use pcb_kicad_sch::{KicadProject, connectivity::ConnectivityGraph};

#[test]
fn issue_24201_reduces_hierarchical_connectivity() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-data/kicad-10/issue24201");
    let project = KicadProject::load(directory).expect("load KiCad 10 hierarchy");

    assert_eq!(project.schematic_files.len(), 2);
    assert_eq!(project.document.pages.len(), 2);

    let graph = ConnectivityGraph::from_kicad(&project.document);
    assert_eq!(graph.components.len(), 2);
    assert!(
        graph
            .components
            .iter()
            .all(|component| component.managed_slot.is_none())
    );
    assert!(graph.groups.iter().all(|group| group.terminals.len() == 2));

    let named_groups = graph
        .groups
        .iter()
        .map(|group| group.names.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        named_groups,
        BTreeSet::from([
            BTreeSet::from(["A".to_string()]),
            BTreeSet::from(["B".to_string()]),
        ])
    );
    assert!(graph.groups.iter().all(|group| group.origins.len() == 2));
}
