mod common;

use std::path::PathBuf;

use pcb_kicad_sch::connectivity::ConnectivityGraph;

#[test]
fn issue_24201_reduces_hierarchical_connectivity() {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test-data/kicad-10/issue24201");
    let project = common::TestProject::load(directory);

    assert_eq!(project.schematic_files.len(), 2);
    assert_eq!(project.document.pages.len(), 2);

    let graph = ConnectivityGraph::from_kicad(&project.document).expect("reduce KiCad project");
    assert_eq!(graph.components.len(), 2);
    assert!(
        graph
            .components
            .iter()
            .all(|component| component.managed_slot.is_none())
    );
    assert!(graph.groups.iter().all(|group| group.terminals.len() == 2));
    assert!(graph.groups.iter().all(|group| group.origins.len() == 2));
}
