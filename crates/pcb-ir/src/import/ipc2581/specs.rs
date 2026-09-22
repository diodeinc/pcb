//! IPC Spec tables and references.

use super::*;

/// Fill the spec tables of a document that has none, in name order.
pub(super) fn populate_ipc_specs(
    doc: &mut GeometryDocument,
    ipc: &Ipc2581,
    specs: &HashMap<Symbol, ipc2581::types::Spec>,
) {
    let mut specs = specs.values().collect::<Vec<_>>();
    specs.sort_by(|left, right| ipc.resolve(left.name).cmp(ipc.resolve(right.name)));

    for spec in specs {
        let item_start = doc.spec_items.len() as u32;
        doc.spec_items
            .extend(spec.items.iter().map(|item| SpecItem {
                kind: map_spec_item_kind(item.kind),
            }));
        doc.specs.push(Spec {
            name: spec.name,
            items: Span::new(item_start, doc.spec_items.len() as u32 - item_start),
        });
    }
}

pub(super) fn map_spec_item_kind(kind: ipc2581::types::ecad::SpecItemKind) -> SpecItemKind {
    match kind {
        ipc2581::types::ecad::SpecItemKind::General => SpecItemKind::General,
        ipc2581::types::ecad::SpecItemKind::Dielectric => SpecItemKind::Dielectric,
        ipc2581::types::ecad::SpecItemKind::Conductor => SpecItemKind::Conductor,
        ipc2581::types::ecad::SpecItemKind::SurfaceFinish => SpecItemKind::SurfaceFinish,
        ipc2581::types::ecad::SpecItemKind::VCut => SpecItemKind::VCut,
        ipc2581::types::ecad::SpecItemKind::Other => SpecItemKind::Other,
    }
}

pub(super) fn push_spec_refs(doc: &mut GeometryDocument, spec_refs: &[Symbol]) -> Span {
    let start = doc.spec_refs.len() as u32;
    doc.spec_refs
        .extend(spec_refs.iter().copied().map(|spec| SpecRef { spec }));
    Span::new(start, doc.spec_refs.len() as u32 - start)
}
