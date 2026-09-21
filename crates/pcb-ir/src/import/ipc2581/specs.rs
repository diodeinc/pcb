//! IPC Spec tables and references.

use super::*;

pub(super) fn populate_ipc_specs(doc: &mut GeometryDocument, ipc: &Ipc2581) {
    let Some(ecad) = ipc.ecad() else {
        return;
    };

    doc.specs.clear();
    doc.spec_items.clear();
    doc.spec_properties.clear();

    let mut specs = ecad.cad_header.specs.values().collect::<Vec<_>>();
    specs.sort_by(|left, right| ipc.resolve(left.name).cmp(ipc.resolve(right.name)));

    for spec in specs {
        let item_start = doc.spec_items.len() as u32;
        for item in &spec.items {
            let property_start = doc.spec_properties.len() as u32;
            doc.spec_properties
                .extend(item.properties.iter().map(|property| SpecProperty {
                    value: property.value,
                    text: property.text,
                    unit: property.unit,
                    plus_tol: property.plus_tol,
                    minus_tol: property.minus_tol,
                    tol_percent: property.tol_percent,
                }));
            doc.spec_items.push(SpecItem {
                element: item.element,
                kind: map_spec_item_kind(item.kind),
                item_type: item.item_type,
                comment: item.comment,
                properties: Span::new(
                    property_start,
                    doc.spec_properties.len() as u32 - property_start,
                ),
            });
        }
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
