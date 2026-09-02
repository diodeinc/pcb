//! Deterministic PCBA assembly reports over the canonical imported design.

use std::collections::{BTreeSet, HashMap};

use anyhow::{Result, bail};
use ipc2581::types::LayerFunction;
use pcb_ir::dialects::assembly as ir;
use pcb_ir::dialects::ipc::{FeatureSpan, LayoutStepKind, PlatingKind};
use pcb_ir::geom::path::{ContourBuf, PathCmd, PathOp};
use pcb_ir::geom::{
    Affine2, BBox, FillRule as IrFillRule, LineCap as IrLineCap, LineJoin as IrLineJoin,
    LinePattern as IrLinePattern, Paint, Point as IrPoint, Polarity as IrPolarity,
};
use pcb_ir::import::ipc2581::{ImportedDesign, LayoutOccurrenceId};
use pcb_ir::import::physical::{
    Association, AssociationBasis as PhysicalAssociationBasis, LandId, PhysicalHoleKind,
    PhysicalTerminationId, PhysicalView,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::LayoutTarget;

pub mod report;

pub use report::AssemblyReport;

/// Build the stable report consumed by native, CLI, and WebAssembly surfaces.
///
/// The report uses source-backed assembly IR and conservative physical
/// relationships. Geometry can establish only a unique exact overlap; the
/// report applies no quote or shop policy.
pub fn build_report(imported: &ImportedDesign, target: LayoutTarget) -> Result<AssemblyReport> {
    let scope = match target {
        LayoutTarget::Board => ir::Scope::Board,
        LayoutTarget::BoardArray => ir::Scope::BoardArray,
    };
    let assembly = imported.assembly_document(scope)?;
    let physical = imported.physical_view(target.artwork_scope())?;
    let mut ids = IdAllocator::default();
    let (profiles, profile_ids) = physical_profiles(&assembly, &mut ids);
    let (scope_bounds, scope_area) = assembly
        .root_step
        .and_then(|step| step_envelope(&assembly.steps[step as usize]))
        .unzip();

    let scoped_packages = assembly
        .occurrences
        .iter()
        .filter_map(|occurrence| assembly.components[occurrence.id.component.0 as usize].package)
        .collect::<BTreeSet<_>>();
    let mut package_ids = HashMap::new();
    let mut packages = assembly
        .packages
        .iter()
        .filter(|package| scoped_packages.contains(&package.id))
        .map(|package| {
            let source_step = assembly.steps[package.step as usize].name.clone();
            let id = ids.allocate("package", &(&source_step, &package.name));
            package_ids.insert(package.id, id.clone());
            report::Package {
                id,
                source_step,
                name: package.name.clone(),
                package_type: package.package_type.clone(),
                pin_one: package.pin_one.clone(),
                pin_one_orientation: package.pin_one_orientation.clone(),
                height_mm: package.height.map(canonical_number),
                negative_body_extension_mm: package.negative_body_extension.map(canonical_number),
                comment: package.comment.clone(),
                pickup_point_mm: package.pickup_point.map(point),
                views: package.views.iter().map(package_view).collect(),
                pins: package.pins.iter().map(package_pin).collect(),
            }
        })
        .collect::<Vec<_>>();
    packages.sort_by(|left, right| left.id.cmp(&right.id));

    let (mut boards, board_ids) =
        board_occurrences(imported, &assembly, target, &profile_ids, &mut ids);
    boards.sort_by(|left, right| left.id.cmp(&right.id));

    let mut component_ids = HashMap::new();
    let mut components = assembly
        .occurrences
        .iter()
        .map(|occurrence| {
            let definition = &assembly.components[occurrence.id.component.0 as usize];
            let source_step = assembly.steps[definition.step as usize].name.clone();
            let layout_path = layout_path(imported, target, occurrence.id.layout, definition.step);
            let transform = affine(occurrence.root_from_component);
            let id = ids.allocate(
                "component",
                &(
                    &source_step,
                    &layout_path,
                    &definition.designator,
                    transform,
                ),
            );
            component_ids.insert(occurrence.id, id.clone());
            let bom = bom_evidence(&assembly, definition);
            let excluded = bom
                .as_ref()
                .is_some_and(|bom| bom.category == Some(report::BomCategory::Document));
            report::Component {
                id,
                board_id: occurrence
                    .board
                    .and_then(|board| board_ids.get(&board).cloned()),
                source_step,
                layout_path,
                reference_designator: definition.designator.clone(),
                part: definition.part.clone(),
                package_id: definition
                    .package
                    .and_then(|package| package_ids.get(&package).cloned()),
                package_ref: definition.package_ref.clone(),
                bom,
                population: population(occurrence.population),
                side: assembly_side(definition.side),
                mount: component_mount(definition.mount),
                assembly_status: if excluded {
                    report::AssemblyStatus::Excluded
                } else {
                    report::AssemblyStatus::Included
                },
                exclusion_reason: excluded.then_some(report::ExclusionReason::DocumentBomCategory),
                transform,
                termination_ids: Vec::new(),
            }
        })
        .collect::<Vec<_>>();

    let lands = physical
        .lands
        .iter()
        .map(|land| (land.id, land))
        .collect::<HashMap<LandId, _>>();
    let mut component_terminations = HashMap::<String, Vec<String>>::new();
    let mut termination_ids = HashMap::<PhysicalTerminationId, String>::new();
    let mut terminations = physical
        .terminations
        .iter()
        .map(|termination| {
            let component_id = component_ids
                .get(&termination.component)
                .expect("physical termination component is in the same assembly scope")
                .clone();
            let pin = imported.resolve(termination.pin).to_owned();
            let padstack = imported.resolve(termination.padstack).to_owned();
            let location_mm = point(termination.at);
            let id = ids.allocate(
                "termination",
                &(&component_id, &pin, &padstack, location_mm),
            );
            termination_ids.insert(termination.id, id.clone());
            component_terminations
                .entry(component_id.clone())
                .or_default()
                .push(id.clone());
            let layers = termination
                .lands
                .iter()
                .map(|land| {
                    let land = lands
                        .get(land)
                        .expect("physical termination land is in the same physical view");
                    imported
                        .resolve(imported.layer_definitions[land.layer.0 as usize].name)
                        .to_owned()
                })
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(|layer| report::LandEvidence { layer })
                .collect();
            let mut paste_islands = physical
                .paste_islands
                .iter()
                .filter(|island| island.termination == Some(termination.id))
                .map(|island| report::PasteEvidence {
                    layer: imported
                        .resolve(imported.layer_definitions[island.layer.0 as usize].name)
                        .to_owned(),
                    side: physical_side(island.side),
                    location_mm: point(island.at),
                })
                .collect::<Vec<_>>();
            paste_islands.sort_by(|left, right| {
                left.layer
                    .cmp(&right.layer)
                    .then_with(|| left.location_mm.x.total_cmp(&right.location_mm.x))
                    .then_with(|| left.location_mm.y.total_cmp(&right.location_mm.y))
            });
            let mut mask_openings = physical
                .mask_openings
                .iter()
                .filter(|opening| {
                    (termination.side == pcb_ir::dialects::Side::None
                        || opening.side == termination.side)
                        && opening
                            .lands
                            .resolved()
                            .is_some_and(|land| termination.lands.contains(land))
                })
                .map(|opening| report::MaskEvidence {
                    layer: imported
                        .resolve(imported.layer_definitions[opening.layer.0 as usize].name)
                        .to_owned(),
                    side: physical_side(opening.side),
                })
                .collect::<Vec<_>>();
            mask_openings.sort_by(|left, right| left.layer.cmp(&right.layer));
            report::Termination {
                id,
                component_id,
                pin,
                pin_type: physical_pin_type(termination.pin_type),
                mount_type: termination.mount_type.map(physical_pin_mount),
                padstack,
                location_mm,
                side: physical_side(termination.side),
                population: population(termination.population),
                lands: layers,
                paste_islands,
                mask_openings,
                hole_ids: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    terminations.sort_by(|left, right| left.id.cmp(&right.id));

    let (mut holes, mut termination_holes) =
        physical_hole_reports(imported, &physical, &board_ids, &termination_ids, &mut ids);
    holes.sort_by(|left, right| left.id.cmp(&right.id));
    for termination in &mut terminations {
        termination.hole_ids = termination_holes
            .remove(&termination.id)
            .unwrap_or_default();
        termination.hole_ids.sort();
    }

    for component in &mut components {
        component.termination_ids = component_terminations
            .remove(&component.id)
            .unwrap_or_default();
        component.termination_ids.sort();
    }
    components.sort_by(|left, right| left.id.cmp(&right.id));

    let mut diagnostics = component_diagnostics(&components, &mut ids);
    diagnostics.extend(hole_diagnostics(&holes, &mut ids));
    diagnostics.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| left.subject.id.cmp(&right.subject.id))
    });
    let readiness = if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == report::DiagnosticSeverity::Error)
    {
        report::Readiness::Incomplete
    } else if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == report::DiagnosticSeverity::Warning)
    {
        report::Readiness::ReviewRequired
    } else {
        report::Readiness::Ready
    };
    let included_populated_component_ids = components
        .iter()
        .filter(|component| {
            component.assembly_status == report::AssemblyStatus::Included
                && component.population == report::Population::Populate
        })
        .map(|component| component.id.as_str())
        .collect::<BTreeSet<_>>();
    let paste_on_included_populated = |island: &&pcb_ir::import::physical::PasteIsland| {
        island
            .component
            .resolved()
            .and_then(|component| component_ids.get(component))
            .is_some_and(|component| included_populated_component_ids.contains(component.as_str()))
    };
    let paste = report::PasteSummary {
        islands: physical.paste_islands.len() as u64,
        exactly_linked_to_termination: physical
            .paste_islands
            .iter()
            .filter(|island| island.termination.is_some())
            .count() as u64,
        on_included_populated_components: physical
            .paste_islands
            .iter()
            .filter(paste_on_included_populated)
            .count() as u64,
        exactly_linked_on_included_populated_components: physical
            .paste_islands
            .iter()
            .filter(paste_on_included_populated)
            .filter(|island| island.termination.is_some())
            .count() as u64,
    };
    let summary = summarize(&boards, &packages, &components, &terminations, paste);

    let report = report::AssemblyReport {
        schema_version: report::REPORT_SCHEMA_VERSION,
        units: report::Units {
            length: "mm",
            angle: "degree",
        },
        source: source(imported),
        scope: report::Scope {
            kind: match target {
                LayoutTarget::Board => report::ScopeKind::Board,
                LayoutTarget::BoardArray => report::ScopeKind::BoardArray,
            },
            root_step: assembly
                .root_step
                .map(|step| assembly.steps[step as usize].name.clone()),
            coordinate_frame: "ipc_2581_design_x_right_y_up",
            profile_ids: assembly
                .root_step
                .and_then(|step| profile_ids.get(&step).cloned())
                .unwrap_or_default(),
            bounds_mm: scope_bounds,
            area_mm2: scope_area,
        },
        readiness,
        summary,
        profiles,
        boards,
        packages,
        components,
        terminations,
        holes,
        diagnostics,
    };
    validate_finite_numbers(&serde_value::to_value(&report)?)?;
    Ok(report)
}

fn validate_finite_numbers(value: &serde_value::Value) -> Result<()> {
    use serde_value::Value;

    match value {
        Value::F32(value) if !value.is_finite() => {
            bail!("assembly report contains a non-finite number")
        }
        Value::F64(value) if !value.is_finite() => {
            bail!("assembly report contains a non-finite number")
        }
        Value::Option(Some(value)) | Value::Newtype(value) => validate_finite_numbers(value),
        Value::Seq(values) => values.iter().try_for_each(validate_finite_numbers),
        Value::Map(values) => values.iter().try_for_each(|(key, value)| {
            validate_finite_numbers(key).and_then(|()| validate_finite_numbers(value))
        }),
        _ => Ok(()),
    }
}

fn source(imported: &ImportedDesign) -> report::Source {
    let history = imported.history_record.as_ref();
    let software_package = history
        .and_then(|history| history.file_revision.as_ref())
        .and_then(|revision| revision.software_package.as_ref())
        .map(|package| report::SoftwarePackage {
            name: imported.resolve(package.name).to_owned(),
            revision: package
                .revision
                .map(|revision| imported.resolve(revision).to_owned()),
            vendor: package
                .vendor
                .map(|vendor| imported.resolve(vendor).to_owned()),
        });
    report::Source {
        format: "ipc_2581",
        revision: imported.revision.clone(),
        creation_software: history
            .and_then(|history| history.software)
            .map(|software| imported.resolve(software).to_owned()),
        software_package,
    }
}

fn physical_profiles(
    assembly: &ir::Document,
    ids: &mut IdAllocator,
) -> (Vec<report::PhysicalProfile>, HashMap<u32, Vec<String>>) {
    let mut profiles = Vec::new();
    let mut profile_ids = HashMap::new();
    for (step_index, step) in assembly.steps.iter().enumerate() {
        for (profile_index, profile) in step.profiles.iter().enumerate() {
            let id = ids.allocate("profile", &(&step.name, profile_index));
            let (profile_bounds, profile_area) = profile_envelope(profile);
            profile_ids
                .entry(step_index as u32)
                .or_insert_with(Vec::new)
                .push(id.clone());
            profiles.push(report::PhysicalProfile {
                id,
                source_step: step.name.clone(),
                bounds_mm: bounds(profile_bounds),
                area_mm2: canonical_number(profile_area),
                outer: contour(&profile.outer),
                cutouts: profile.cutouts.iter().map(contour).collect(),
            });
        }
    }
    (profiles, profile_ids)
}

fn step_envelope(step: &ir::Step) -> Option<(report::Bounds, f64)> {
    step.profiles
        .iter()
        .map(profile_envelope)
        .reduce(
            |(combined_bounds, combined_area), (profile_bounds, profile_area)| {
                (
                    combined_bounds.union(profile_bounds),
                    combined_area + profile_area,
                )
            },
        )
        .map(|(profile_bounds, area)| (bounds(profile_bounds), canonical_number(area)))
}

fn profile_envelope(profile: &ir::Profile) -> (BBox, f64) {
    let cutout_area = profile
        .cutouts
        .iter()
        .map(|cutout| cutout.signed_area().abs())
        .sum::<f64>();
    (
        profile.outer.bbox,
        profile.outer.signed_area().abs() - cutout_area,
    )
}

fn bounds(value: BBox) -> report::Bounds {
    report::Bounds {
        min: point(value.min),
        max: point(value.max),
        width: canonical_number(value.width()),
        height: canonical_number(value.height()),
    }
}

fn board_occurrences(
    imported: &ImportedDesign,
    assembly: &ir::Document,
    target: LayoutTarget,
    profile_ids: &HashMap<u32, Vec<String>>,
    ids: &mut IdAllocator,
) -> (
    Vec<report::BoardOccurrence>,
    HashMap<LayoutOccurrenceId, String>,
) {
    let mut boards = Vec::new();
    let mut board_ids = HashMap::new();
    match target {
        LayoutTarget::Board => {
            if let Some(step) = assembly.root_step {
                let path = root_path(imported, step);
                let transform = affine(Affine2::IDENTITY);
                let (bounds_mm, area_mm2) = step_envelope(&assembly.steps[step as usize]).unzip();
                let id = ids.allocate("board", &(&path, transform));
                board_ids.insert(LayoutOccurrenceId::Root, id.clone());
                boards.push(report::BoardOccurrence {
                    id,
                    step: assembly.steps[step as usize].name.clone(),
                    path,
                    profile_ids: profile_ids.get(&step).cloned().unwrap_or_default(),
                    bounds_mm,
                    area_mm2,
                    transform,
                });
            }
        }
        LayoutTarget::BoardArray => {
            let layout = &imported.geometry.layout;
            if let Some(root) = layout
                .root_step
                .filter(|root| layout.steps[*root as usize].kind == LayoutStepKind::Board)
            {
                let path = root_path(imported, root);
                let transform = affine(Affine2::IDENTITY);
                let (bounds_mm, area_mm2) = step_envelope(&assembly.steps[root as usize]).unzip();
                let id = ids.allocate("board", &(&path, transform));
                board_ids.insert(LayoutOccurrenceId::Root, id.clone());
                boards.push(report::BoardOccurrence {
                    id,
                    step: imported
                        .resolve(layout.steps[root as usize].source_step_ref)
                        .to_owned(),
                    path,
                    profile_ids: profile_ids.get(&root).cloned().unwrap_or_default(),
                    bounds_mm,
                    area_mm2,
                    transform,
                });
            }
            for (index, instance) in layout.instances.iter().enumerate().filter(|(_, instance)| {
                layout.steps[instance.child_step as usize].kind == LayoutStepKind::Board
            }) {
                let occurrence = LayoutOccurrenceId::Instance(index as u32);
                let path = layout_path(imported, target, occurrence, instance.child_step);
                let transform = affine(instance.transform);
                let (bounds_mm, area_mm2) =
                    step_envelope(&assembly.steps[instance.child_step as usize]).unzip();
                let id = ids.allocate("board", &(&path, transform));
                board_ids.insert(occurrence, id.clone());
                boards.push(report::BoardOccurrence {
                    id,
                    step: imported.resolve(instance.source_step_ref).to_owned(),
                    path,
                    profile_ids: profile_ids
                        .get(&instance.child_step)
                        .cloned()
                        .unwrap_or_default(),
                    bounds_mm,
                    area_mm2,
                    transform,
                });
            }
        }
    }
    (boards, board_ids)
}

fn layout_path(
    imported: &ImportedDesign,
    target: LayoutTarget,
    occurrence: LayoutOccurrenceId,
    board_step: u32,
) -> Vec<report::LayoutPathSegment> {
    if target == LayoutTarget::Board {
        return root_path(imported, board_step);
    }
    let layout = &imported.geometry.layout;
    let LayoutOccurrenceId::Instance(mut instance_index) = occurrence else {
        return root_path(
            imported,
            layout
                .root_step
                .expect("imported design has a canonical layout root"),
        );
    };
    let mut instances = Vec::new();
    loop {
        let instance = &layout.instances[instance_index as usize];
        instances.push(instance);
        let Some(parent) = instance.parent_instance else {
            break;
        };
        instance_index = parent;
    }
    instances.reverse();
    let mut path = root_path(
        imported,
        layout
            .root_step
            .expect("imported design has a canonical layout root"),
    );
    path.extend(instances.into_iter().map(|instance| {
        let repeat = &layout.repeats[instance.repeat as usize];
        report::LayoutPathSegment {
            step: imported.resolve(instance.source_step_ref).to_owned(),
            repeat: Some(report::RepeatPosition {
                index_x: instance.repeat_index_x,
                index_y: instance.repeat_index_y,
                first_x_mm: canonical_number(repeat.x),
                first_y_mm: canonical_number(repeat.y),
                pitch_x_mm: canonical_number(repeat.dx),
                pitch_y_mm: canonical_number(repeat.dy),
                rotation_degrees: canonical_number(repeat.angle),
                mirror: repeat.mirror,
            }),
        }
    }));
    path
}

fn root_path(imported: &ImportedDesign, step: u32) -> Vec<report::LayoutPathSegment> {
    vec![report::LayoutPathSegment {
        step: imported
            .resolve(imported.geometry.layout.steps[step as usize].source_step_ref)
            .to_owned(),
        repeat: None,
    }]
}

fn bom_evidence(
    assembly: &ir::Document,
    component: &ir::ComponentDefinition,
) -> Option<report::BomEvidence> {
    let reference = assembly.preferred_bom_reference(component)?;
    let bom = &assembly.boms[reference.bom as usize];
    let item = assembly.bom_item(reference);
    let mut approved_parts = assembly
        .avl
        .iter()
        .flat_map(|avl| &avl.items)
        .filter(|avl| avl.oem_design_number == item.oem_design_number)
        .flat_map(|avl| &avl.alternatives)
        .map(|part| report::ApprovedPart {
            external_vendor: part.external_vendor.clone(),
            external_mpn: part.external_mpn.clone(),
            qualified: part.qualified,
            chosen: part.chosen,
            manufacturer_part_numbers: part
                .manufacturer_parts
                .iter()
                .map(|part| part.name.clone())
                .collect(),
            vendor_refs: part.vendor_refs.clone(),
        })
        .collect::<Vec<_>>();
    approved_parts.sort_by(|left, right| {
        left.external_vendor
            .cmp(&right.external_vendor)
            .then_with(|| left.external_mpn.cmp(&right.external_mpn))
            .then_with(|| {
                left.manufacturer_part_numbers
                    .cmp(&right.manufacturer_part_numbers)
            })
            .then_with(|| left.vendor_refs.cmp(&right.vendor_refs))
    });
    Some(report::BomEvidence {
        bom: bom.name.clone(),
        oem_design_number: item.oem_design_number.clone(),
        category: item.category.map(bom_category),
        quantity: item.quantity,
        quantity_source: item.quantity_raw.clone(),
        pin_count: item.pin_count,
        pin_count_source: item.pin_count_raw.clone(),
        internal_part_number: item.internal_part_number.clone(),
        approved_parts,
    })
}

fn physical_hole_reports(
    imported: &ImportedDesign,
    physical: &PhysicalView,
    board_ids: &HashMap<LayoutOccurrenceId, String>,
    termination_ids: &HashMap<PhysicalTerminationId, String>,
    ids: &mut IdAllocator,
) -> (Vec<report::Hole>, HashMap<String, Vec<String>>) {
    let mut termination_holes = HashMap::<String, Vec<String>>::new();
    let holes = physical
        .holes
        .iter()
        .map(|hole| {
            let source_layer = imported
                .resolve(imported.layer_definitions[hole.layer.0 as usize].name)
                .to_owned();
            let source_name = hole
                .source_name
                .map(|name| imported.resolve(name).to_owned());
            let board_id = hole.board.and_then(|board| board_ids.get(&board).cloned());
            let location_mm = point(hole.at);
            let finished_diameter_mm = hole.finished_diameter.map(canonical_number);
            let kind = match hole.kind {
                PhysicalHoleKind::Round => report::HoleKind::Round,
                PhysicalHoleKind::Slot => report::HoleKind::Slot,
            };
            let plating = hole_plating(hole.plating);
            let padstack = hole
                .padstack
                .map(|padstack| imported.resolve(padstack).to_owned());
            let net = hole.net.map(|net| imported.resolve(net).to_owned());
            let span = hole_span(imported, hole.span);
            let source_specs = hole
                .spec_refs
                .iter()
                .map(|spec| imported.resolve(*spec).to_owned())
                .collect::<Vec<_>>();
            let termination =
                termination_association(&hole.termination, hole.termination_basis, termination_ids);
            let id = ids.allocate(
                "hole",
                &(
                    &board_id,
                    &source_layer,
                    &source_name,
                    kind,
                    location_mm,
                    finished_diameter_mm,
                    plating,
                    &padstack,
                    &net,
                    &span,
                    &source_specs,
                    &termination,
                ),
            );
            if let Association::Resolved(termination) = hole.termination
                && let Some(termination_id) = termination_ids.get(&termination)
            {
                termination_holes
                    .entry(termination_id.clone())
                    .or_default()
                    .push(id.clone());
            }
            report::Hole {
                id: id.clone(),
                board_id,
                source_layer,
                source_name,
                kind,
                location_mm,
                finished_diameter_mm,
                plating,
                padstack,
                net,
                span,
                termination,
                protection: protection_intent(imported, hole, &id, ids),
            }
        })
        .collect();
    (holes, termination_holes)
}

fn termination_association(
    association: &Association<PhysicalTerminationId>,
    basis: Option<PhysicalAssociationBasis>,
    termination_ids: &HashMap<PhysicalTerminationId, String>,
) -> report::TerminationAssociation {
    let (status, physical_ids) = match association {
        Association::Resolved(termination) => (
            match basis {
                Some(PhysicalAssociationBasis::SourceIdentity) => {
                    report::AssociationStatus::Explicit
                }
                Some(PhysicalAssociationBasis::ExactGeometry) => {
                    report::AssociationStatus::ExactGeometric
                }
                None => report::AssociationStatus::Unsupported,
            },
            vec![*termination],
        ),
        Association::Ambiguous(terminations) => {
            (report::AssociationStatus::Ambiguous, terminations.clone())
        }
        Association::Conflicting(terminations) => {
            (report::AssociationStatus::Conflicting, terminations.clone())
        }
        Association::Unresolved => (report::AssociationStatus::Unresolved, Vec::new()),
    };
    let mut termination_ids = physical_ids
        .into_iter()
        .filter_map(|termination| termination_ids.get(&termination).cloned())
        .collect::<Vec<_>>();
    termination_ids.sort();
    report::TerminationAssociation {
        status,
        basis: basis.map(|basis| match basis {
            PhysicalAssociationBasis::SourceIdentity => report::AssociationBasis::SourceIdentity,
            PhysicalAssociationBasis::ExactGeometry => report::AssociationBasis::ExactGeometry,
        }),
        termination_ids,
    }
}

fn protection_intent(
    imported: &ImportedDesign,
    hole: &pcb_ir::import::physical::PhysicalHole,
    hole_id: &str,
    ids: &mut IdAllocator,
) -> report::ProtectionIntent {
    let source_layer = imported
        .resolve(imported.layer_definitions[hole.layer.0 as usize].name)
        .to_owned();
    let mut evidence = Vec::new();
    let source_terms = spec_terms(imported, &hole.spec_refs);
    if has_protection_action(&source_terms) {
        evidence.push(report::ProtectionEvidence {
            id: ids.allocate(
                "protection-evidence",
                &(hole_id, "SOURCE_TERMS", &source_layer, &source_terms),
            ),
            kind: report::ProtectionEvidenceKind::SourceTerms,
            layer: source_layer.clone(),
            side: report::Side::None,
            span: hole_span(imported, hole.span),
            specs: hole
                .spec_refs
                .iter()
                .map(|spec| imported.resolve(*spec).to_owned())
                .collect(),
            terms: source_terms,
        });
    }
    if hole.plating == PlatingKind::ViaCapped {
        evidence.push(report::ProtectionEvidence {
            id: ids.allocate(
                "protection-evidence",
                &(hole_id, "VIA_CAPPED", &source_layer),
            ),
            kind: report::ProtectionEvidenceKind::ViaCappedPlatingStatus,
            layer: source_layer,
            side: report::Side::None,
            span: hole_span(imported, hole.span),
            specs: Vec::new(),
            terms: vec!["VIA_CAPPED".to_owned()],
        });
    }
    evidence.extend(hole.protection.iter().filter_map(|source| {
        let layer = imported
            .resolve(imported.layer_definitions[source.layer.0 as usize].name)
            .to_owned();
        let specs = source
            .spec_refs
            .iter()
            .map(|spec| imported.resolve(*spec).to_owned())
            .collect::<Vec<_>>();
        let terms = spec_terms(imported, &source.spec_refs);
        let kind = match source.function {
            LayerFunction::HoleFill => report::ProtectionEvidenceKind::HoleFillLayer,
            LayerFunction::CoatingCond | LayerFunction::CoatingNonCond
                if has_protection_action(&terms) =>
            {
                report::ProtectionEvidenceKind::SourceTerms
            }
            LayerFunction::CoatingCond | LayerFunction::CoatingNonCond => return None,
            _ => unreachable!("physical protection uses a protection layer"),
        };
        Some(report::ProtectionEvidence {
            id: ids.allocate(
                "protection-evidence",
                &(hole_id, source.function.as_str(), &layer, &specs, &terms),
            ),
            kind,
            layer,
            side: physical_side(source.side),
            span: hole_span(imported, source.span),
            specs,
            terms,
        })
    }));

    let mut methods = BTreeSet::new();
    let mut fill_materials = BTreeSet::new();
    for source in &evidence {
        let text = normalized_terms(&source.terms);
        if contains_term(&text, OPEN_TERMS) {
            methods.insert(report::ProtectionMethod::Open);
        }
        if contains_term(&text, TENT_TERMS) {
            methods.insert(report::ProtectionMethod::Tented);
        }
        if contains_term(&text, PLUG_TERMS) {
            methods.insert(report::ProtectionMethod::Plugged);
        }
        if contains_term(&text, FILL_TERMS) {
            methods.insert(report::ProtectionMethod::Filled);
        }
        if contains_term(&text, CAP_TERMS) {
            methods.insert(report::ProtectionMethod::Capped);
        }
        match source.kind {
            report::ProtectionEvidenceKind::SourceTerms => {}
            report::ProtectionEvidenceKind::ViaCappedPlatingStatus => {
                methods.insert(report::ProtectionMethod::Capped);
            }
            report::ProtectionEvidenceKind::HoleFillLayer => {
                methods.insert(if contains_term(&text, PLUG_TERMS) {
                    report::ProtectionMethod::Plugged
                } else {
                    report::ProtectionMethod::Filled
                });
            }
        }
        let has_fill = source.kind == report::ProtectionEvidenceKind::HoleFillLayer
            || contains_term(&text, FILL_TERMS)
            || contains_term(&text, PLUG_TERMS);
        if has_fill {
            if text.contains(" NON CONDUCTIVE ") || text.contains(" NONCONDUCTIVE ") {
                fill_materials.insert(report::FillMaterial::NonConductive);
            } else if text.contains(" COPPER ") {
                fill_materials.insert(report::FillMaterial::Copper);
            } else if text.contains(" CONDUCTIVE ") {
                fill_materials.insert(report::FillMaterial::Conductive);
            }
        }
    }
    let conflicting = fill_materials.len() > 1
        || (methods.contains(&report::ProtectionMethod::Open) && methods.len() > 1);
    report::ProtectionIntent {
        status: if evidence.is_empty() {
            report::ProtectionStatus::Unknown
        } else if conflicting {
            report::ProtectionStatus::Conflicting
        } else {
            report::ProtectionStatus::Explicit
        },
        methods: methods.into_iter().collect(),
        fill_material: (fill_materials.len() == 1)
            .then(|| *fill_materials.first().expect("one fill material")),
        evidence,
    }
}

fn spec_terms(imported: &ImportedDesign, spec_refs: &[ipc2581::Symbol]) -> Vec<String> {
    let mut terms = BTreeSet::new();
    for spec_ref in spec_refs {
        let Some(spec) = imported.specs.get(spec_ref) else {
            continue;
        };
        terms.extend(
            spec.material
                .into_iter()
                .chain(spec.properties.iter().copied())
                .map(|term| imported.resolve(term).to_owned()),
        );
        for item in &spec.items {
            terms.extend(
                item.properties
                    .iter()
                    .filter_map(|property| property.text)
                    .map(|term| imported.resolve(term).to_owned()),
            );
        }
    }
    terms.into_iter().collect()
}

fn normalized_terms(terms: &[String]) -> String {
    format!(
        " {} ",
        terms
            .join(" ")
            .to_ascii_uppercase()
            .replace(['-', '_'], " ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    )
}

fn has_protection_action(terms: &[String]) -> bool {
    let text = normalized_terms(terms);
    [OPEN_TERMS, TENT_TERMS, PLUG_TERMS, FILL_TERMS, CAP_TERMS]
        .into_iter()
        .any(|terms| contains_term(&text, terms))
}

const OPEN_TERMS: &[&str] = &["OPEN", "OPENED"];
const TENT_TERMS: &[&str] = &["TENT", "TENTED", "TENTING"];
const PLUG_TERMS: &[&str] = &["PLUG", "PLUGGED", "PLUGGING"];
const FILL_TERMS: &[&str] = &["FILL", "FILLED", "FILLING"];
const CAP_TERMS: &[&str] = &["CAP", "CAPPED", "CAPPING"];

fn contains_term(text: &str, terms: &[&str]) -> bool {
    text.split_ascii_whitespace()
        .any(|word| terms.contains(&word))
}

fn hole_plating(value: PlatingKind) -> report::HolePlating {
    match value {
        PlatingKind::Unknown => report::HolePlating::Unknown,
        PlatingKind::None => report::HolePlating::None,
        PlatingKind::Plated => report::HolePlating::Plated,
        PlatingKind::NonPlated => report::HolePlating::NonPlated,
        PlatingKind::Via => report::HolePlating::Via,
        PlatingKind::ViaCapped => report::HolePlating::ViaCapped,
    }
}

fn hole_span(imported: &ImportedDesign, value: FeatureSpan<ipc2581::Symbol>) -> report::HoleSpan {
    match value {
        FeatureSpan::Unknown => report::HoleSpan {
            kind: report::HoleSpanKind::Unknown,
            from_layer: None,
            to_layer: None,
        },
        FeatureSpan::Layer(layer) => report::HoleSpan {
            kind: report::HoleSpanKind::Layer,
            from_layer: Some(imported.resolve(layer).to_owned()),
            to_layer: None,
        },
        FeatureSpan::ThroughBoard => report::HoleSpan {
            kind: report::HoleSpanKind::ThroughBoard,
            from_layer: None,
            to_layer: None,
        },
        FeatureSpan::FromTo { from, to } => report::HoleSpan {
            kind: report::HoleSpanKind::FromTo,
            from_layer: from.map(|layer| imported.resolve(layer).to_owned()),
            to_layer: to.map(|layer| imported.resolve(layer).to_owned()),
        },
    }
}

fn component_diagnostics(
    components: &[report::Component],
    ids: &mut IdAllocator,
) -> Vec<report::Diagnostic> {
    let mut diagnostics = Vec::new();
    for component in components
        .iter()
        .filter(|component| component.assembly_status == report::AssemblyStatus::Included)
    {
        let label = component
            .reference_designator
            .as_deref()
            .unwrap_or(component.id.as_str());
        match component.population {
            report::Population::Unspecified => diagnostics.push(diagnostic(
                ids,
                component,
                report::DiagnosticCode::MissingPopulation,
                format!("component '{label}' has no explicit population state"),
            )),
            report::Population::Conflicting => diagnostics.push(diagnostic(
                ids,
                component,
                report::DiagnosticCode::ConflictingPopulation,
                format!("component '{label}' has conflicting population states"),
            )),
            report::Population::Populate | report::Population::DoNotPopulate => {}
        }
        if component.reference_designator.is_none() {
            diagnostics.push(diagnostic(
                ids,
                component,
                report::DiagnosticCode::MissingReferenceDesignator,
                "assembly component has no reference designator".to_owned(),
            ));
        }
        if component.population == report::Population::Populate && component.package_id.is_none() {
            diagnostics.push(diagnostic(
                ids,
                component,
                report::DiagnosticCode::MissingPackage,
                format!("populated component '{label}' has no resolved package"),
            ));
        }
        if component.population == report::Population::Populate
            && matches!(
                component.mount,
                report::ComponentMount::Smt | report::ComponentMount::ThroughHole
            )
            && component.termination_ids.is_empty()
        {
            diagnostics.push(diagnostic(
                ids,
                component,
                report::DiagnosticCode::MissingPhysicalTerminations,
                format!(
                    "populated solder-mounted component '{label}' has no exact physical terminations"
                ),
            ));
        }
    }
    diagnostics
}

fn hole_diagnostics(holes: &[report::Hole], ids: &mut IdAllocator) -> Vec<report::Diagnostic> {
    let mut diagnostics = Vec::new();
    for hole in holes {
        let association_code = match hole.termination.status {
            report::AssociationStatus::Ambiguous => {
                Some(report::DiagnosticCode::AmbiguousHoleTermination)
            }
            report::AssociationStatus::Conflicting => {
                Some(report::DiagnosticCode::ConflictingHoleTermination)
            }
            _ => None,
        };
        if let Some(code) = association_code {
            diagnostics.push(hole_diagnostic(
                ids,
                hole,
                code,
                "hole cannot be associated with one physical component termination".to_owned(),
            ));
        }
        let component_land_via = matches!(
            hole.termination.status,
            report::AssociationStatus::Explicit | report::AssociationStatus::ExactGeometric
        ) && matches!(
            hole.plating,
            report::HolePlating::Via | report::HolePlating::ViaCapped
        );
        if component_land_via {
            let protection = match hole.protection.status {
                report::ProtectionStatus::Unknown => Some((
                    report::DiagnosticCode::UnknownViaProtection,
                    "component-land via has no explicit protection intent",
                )),
                report::ProtectionStatus::Conflicting => Some((
                    report::DiagnosticCode::ConflictingViaProtection,
                    "component-land via has conflicting protection intent",
                )),
                report::ProtectionStatus::Explicit => None,
            };
            if let Some((code, message)) = protection {
                diagnostics.push(hole_diagnostic(ids, hole, code, message.to_owned()));
            }
        }
    }
    diagnostics
}

fn hole_diagnostic(
    ids: &mut IdAllocator,
    hole: &report::Hole,
    code: report::DiagnosticCode,
    message: String,
) -> report::Diagnostic {
    report::Diagnostic {
        id: ids.allocate("assembly-diagnostic", &(code, &hole.id)),
        severity: report::DiagnosticSeverity::Warning,
        code,
        subject: report::DiagnosticSubject {
            kind: report::DiagnosticSubjectKind::Hole,
            id: hole.id.clone(),
            reference_designator: None,
        },
        message,
    }
}

fn diagnostic(
    ids: &mut IdAllocator,
    component: &report::Component,
    code: report::DiagnosticCode,
    message: String,
) -> report::Diagnostic {
    report::Diagnostic {
        id: ids.allocate("assembly-diagnostic", &(code, &component.id)),
        severity: report::DiagnosticSeverity::Error,
        code,
        subject: report::DiagnosticSubject {
            kind: report::DiagnosticSubjectKind::Component,
            id: component.id.clone(),
            reference_designator: component.reference_designator.clone(),
        },
        message,
    }
}

fn summarize(
    boards: &[report::BoardOccurrence],
    packages: &[report::Package],
    components: &[report::Component],
    terminations: &[report::Termination],
    paste: report::PasteSummary,
) -> report::Summary {
    let included = components
        .iter()
        .filter(|component| component.assembly_status == report::AssemblyStatus::Included)
        .collect::<Vec<_>>();
    let populated_component_ids = included
        .iter()
        .filter(|component| component.population == report::Population::Populate)
        .map(|component| component.id.as_str())
        .collect::<BTreeSet<_>>();
    report::Summary {
        board_occurrences: boards.len() as u64,
        packages: packages.len() as u64,
        components: report::ComponentSummary {
            total: components.len() as u64,
            included: included.len() as u64,
            excluded: (components.len() - included.len()) as u64,
            included_populated: included
                .iter()
                .filter(|component| component.population == report::Population::Populate)
                .count() as u64,
            included_do_not_populate: included
                .iter()
                .filter(|component| component.population == report::Population::DoNotPopulate)
                .count() as u64,
            included_population_unresolved: included
                .iter()
                .filter(|component| {
                    matches!(
                        component.population,
                        report::Population::Unspecified | report::Population::Conflicting
                    )
                })
                .count() as u64,
        },
        terminations: report::TerminationSummary {
            total: terminations.len() as u64,
            on_included_populated_components: terminations
                .iter()
                .filter(|termination| {
                    populated_component_ids.contains(termination.component_id.as_str())
                })
                .count() as u64,
            surface_on_included_populated_components: terminations
                .iter()
                .filter(|termination| {
                    populated_component_ids.contains(termination.component_id.as_str())
                        && termination.pin_type == report::PinType::Surface
                })
                .count() as u64,
            through_on_included_populated_components: terminations
                .iter()
                .filter(|termination| {
                    populated_component_ids.contains(termination.component_id.as_str())
                        && termination.pin_type == report::PinType::Through
                })
                .count() as u64,
            blind_on_included_populated_components: terminations
                .iter()
                .filter(|termination| {
                    populated_component_ids.contains(termination.component_id.as_str())
                        && termination.pin_type == report::PinType::Blind
                })
                .count() as u64,
        },
        paste,
    }
}

fn package_view(view: &ir::PackageView) -> report::PackageView {
    report::PackageView {
        kind: match view.kind {
            ir::PackageViewKind::Primary => report::PackageViewKind::Primary,
            ir::PackageViewKind::Topside => report::PackageViewKind::Topside,
            ir::PackageViewKind::OtherSide => report::PackageViewKind::OtherSide,
        },
        outline: view.outline.as_ref().map(package_outline),
        land_pattern: view
            .land_pattern
            .as_ref()
            .map(|land_pattern| report::PackageLandPattern {
                pads: land_pattern
                    .pads
                    .iter()
                    .map(|pad| report::PackagePad {
                        padstack_ref: pad.padstack_ref.clone(),
                        x_mm: pad.x.map(canonical_number),
                        y_mm: pad.y.map(canonical_number),
                        transform: pad.transform.map(source_transform),
                        graphic: pad.graphic.as_ref().map(package_graphic),
                        pin_ref: pad.pin_ref.as_ref().map(|pin| report::PackagePinReference {
                            component_ref: pin.component_ref.clone(),
                            pin: pin.pin.clone(),
                            title: pin.title.clone(),
                        }),
                    })
                    .collect(),
                targets: land_pattern
                    .targets
                    .iter()
                    .map(|target| report::PackageTarget {
                        location_mm: point(target.location),
                        transform: target.transform.map(source_transform),
                        shape: package_shape(&target.shape),
                    })
                    .collect(),
            }),
        silkscreen: view
            .silkscreen
            .as_ref()
            .map(|silkscreen| report::PackageSilkscreen {
                outlines: silkscreen.outlines.iter().map(package_outline).collect(),
                markings: silkscreen.markings.iter().map(package_marking).collect(),
            }),
        assembly_drawing: view.assembly_drawing.as_ref().map(|drawing| {
            report::PackageAssemblyDrawing {
                outline: drawing.outline.as_ref().map(package_outline),
                markings: drawing.markings.iter().map(package_marking).collect(),
            }
        }),
    }
}

fn package_marking(marking: &ir::PackageMarking) -> report::PackageMarking {
    report::PackageMarking {
        usage: marking.usage.clone(),
        location_mm: marking.location.map(point),
        transform: marking.transform.map(source_transform),
        graphic: package_graphic(&marking.graphic),
    }
}

fn package_outline(outline: &ir::PackageOutline) -> report::PackageOutline {
    report::PackageOutline {
        transform: outline.transform.map(source_transform),
        shape: package_shape(&outline.shape),
    }
}

fn package_graphic(graphic: &ir::PackageGraphic) -> report::PackageGraphic {
    match graphic {
        ir::PackageGraphic::Shape(shape) => report::PackageGraphic::Shape(package_shape(shape)),
        ir::PackageGraphic::Text(text) => report::PackageGraphic::Text(report::PackageText {
            text: text.text.clone(),
            font_size: text.font_size,
            font_size_source: text.font_size_raw.clone(),
            transform: text.transform.map(source_transform),
            bounds_mm: bounds(BBox::new(text.lower_left, text.upper_right)),
            font_ref: text.font_ref.clone(),
        }),
        ir::PackageGraphic::Outline(outline) => {
            report::PackageGraphic::Outline(package_outline(outline))
        }
    }
}

fn package_shape(shape: &ir::PackageShape) -> report::PackageShape {
    report::PackageShape {
        status: match shape.status {
            ir::PackageGeometryStatus::Complete => report::PackageGeometryStatus::Complete,
            ir::PackageGeometryStatus::Partial => report::PackageGeometryStatus::Partial,
            ir::PackageGeometryStatus::Unresolved => report::PackageGeometryStatus::Unresolved,
            ir::PackageGeometryStatus::Unsupported => report::PackageGeometryStatus::Unsupported,
        },
        references: shape
            .references
            .iter()
            .map(|reference| report::PackageGeometryReference {
                kind: match reference.kind {
                    ir::PackageGeometryReferenceKind::StandardPrimitive => {
                        report::PackageGeometryReferenceKind::StandardPrimitive
                    }
                    ir::PackageGeometryReferenceKind::UserPrimitive => {
                        report::PackageGeometryReferenceKind::UserPrimitive
                    }
                    ir::PackageGeometryReferenceKind::LineDescription => {
                        report::PackageGeometryReferenceKind::LineDescription
                    }
                    ir::PackageGeometryReferenceKind::FillDescription => {
                        report::PackageGeometryReferenceKind::FillDescription
                    }
                },
                id: reference.id.clone(),
            })
            .collect(),
        polarity: match shape.polarity {
            IrPolarity::Dark => report::GeometryPolarity::Dark,
            IrPolarity::Clear => report::GeometryPolarity::Clear,
        },
        paths: shape
            .paths
            .iter()
            .map(|path| report::GeometryPath {
                paint: path_paint(path.paint),
                contours: path.contours.iter().map(contour).collect(),
            })
            .collect(),
    }
}

fn path_paint(paint: Paint) -> report::PathPaint {
    match paint {
        Paint::None => report::PathPaint::None,
        Paint::Fill { rule } => report::PathPaint::Fill {
            rule: match rule {
                IrFillRule::NonZero => report::FillRule::NonZero,
                IrFillRule::EvenOdd => report::FillRule::EvenOdd,
            },
        },
        Paint::Stroke(stroke) => report::PathPaint::Stroke {
            width_mm: canonical_number(stroke.width),
            cap: match stroke.cap {
                IrLineCap::Round => report::LineCap::Round,
                IrLineCap::Square => report::LineCap::Square,
                IrLineCap::Butt => report::LineCap::Butt,
            },
            join: match stroke.join {
                IrLineJoin::Round => report::LineJoin::Round,
                IrLineJoin::Miter => report::LineJoin::Miter,
                IrLineJoin::Bevel => report::LineJoin::Bevel,
            },
            pattern: match stroke.pattern {
                IrLinePattern::Solid => report::LinePattern::Solid,
                IrLinePattern::Dotted => report::LinePattern::Dotted,
                IrLinePattern::Dashed => report::LinePattern::Dashed,
                IrLinePattern::Center => report::LinePattern::Center,
                IrLinePattern::Phantom => report::LinePattern::Phantom,
                IrLinePattern::Erase => report::LinePattern::Erase,
            },
        },
    }
}

fn package_pin(pin: &ir::PackagePin) -> report::PackagePin {
    report::PackagePin {
        view: match pin.view {
            ir::PackagePinView::Primary => report::PackagePinView::Primary,
            ir::PackagePinView::Topside => report::PackagePinView::Topside,
        },
        number: pin.number.clone(),
        name: pin.name.clone(),
        pin_type: match pin.pin_type {
            ir::PackagePinType::Through => report::PinType::Through,
            ir::PackagePinType::Blind => report::PinType::Blind,
            ir::PackagePinType::Surface => report::PinType::Surface,
        },
        electrical_type: pin.electrical_type.map(|value| match value {
            ir::PackagePinElectricalType::Electrical => report::PinElectricalType::Electrical,
            ir::PackagePinElectricalType::Mechanical => report::PinElectricalType::Mechanical,
            ir::PackagePinElectricalType::Undefined => report::PinElectricalType::Undefined,
        }),
        mount_type: pin.mount_type.map(|value| match value {
            ir::PackagePinMountType::SurfaceMountPin => report::PinMountType::SurfaceMountPin,
            ir::PackagePinMountType::SurfaceMountPad => report::PinMountType::SurfaceMountPad,
            ir::PackagePinMountType::ThroughHolePin => report::PinMountType::ThroughHolePin,
            ir::PackagePinMountType::ThroughHoleHole => report::PinMountType::ThroughHoleHole,
            ir::PackagePinMountType::PressFit => report::PinMountType::PressFit,
            ir::PackagePinMountType::NonBoard => report::PinMountType::NonBoard,
            ir::PackagePinMountType::Hole => report::PinMountType::Hole,
            ir::PackagePinMountType::WireBond => report::PinMountType::WireBond,
            ir::PackagePinMountType::Undefined => report::PinMountType::Undefined,
        }),
        polarity: pin.polarity.map(|value| match value {
            ir::PackagePinPolarity::Plus => report::PinPolarity::Plus,
            ir::PackagePinPolarity::Minus => report::PinPolarity::Minus,
            ir::PackagePinPolarity::Anode => report::PinPolarity::Anode,
            ir::PackagePinPolarity::Cathode => report::PinPolarity::Cathode,
        }),
        location_mm: pin.location.map(point),
        transform: pin.transform.map(source_transform),
        shape: package_shape(&pin.shape),
    }
}

fn physical_pin_type(value: ipc2581::types::PackagePinType) -> report::PinType {
    match value {
        ipc2581::types::PackagePinType::Through => report::PinType::Through,
        ipc2581::types::PackagePinType::Blind => report::PinType::Blind,
        ipc2581::types::PackagePinType::Surface => report::PinType::Surface,
    }
}

fn physical_pin_mount(value: ipc2581::types::PackagePinMountType) -> report::PinMountType {
    match value {
        ipc2581::types::PackagePinMountType::SurfaceMountPin => {
            report::PinMountType::SurfaceMountPin
        }
        ipc2581::types::PackagePinMountType::SurfaceMountPad => {
            report::PinMountType::SurfaceMountPad
        }
        ipc2581::types::PackagePinMountType::ThroughHolePin => report::PinMountType::ThroughHolePin,
        ipc2581::types::PackagePinMountType::ThroughHoleHole => {
            report::PinMountType::ThroughHoleHole
        }
        ipc2581::types::PackagePinMountType::PressFit => report::PinMountType::PressFit,
        ipc2581::types::PackagePinMountType::NonBoard => report::PinMountType::NonBoard,
        ipc2581::types::PackagePinMountType::Hole => report::PinMountType::Hole,
        ipc2581::types::PackagePinMountType::WireBond => report::PinMountType::WireBond,
        ipc2581::types::PackagePinMountType::Undefined => report::PinMountType::Undefined,
    }
}

fn population(value: ir::Population) -> report::Population {
    match value {
        ir::Population::Unspecified => report::Population::Unspecified,
        ir::Population::Populate => report::Population::Populate,
        ir::Population::DoNotPopulate => report::Population::DoNotPopulate,
        ir::Population::Conflicting => report::Population::Conflicting,
    }
}

fn assembly_side(value: ir::Side) -> report::Side {
    match value {
        ir::Side::Top => report::Side::Top,
        ir::Side::Bottom => report::Side::Bottom,
        ir::Side::Both => report::Side::Both,
        ir::Side::Internal => report::Side::Internal,
        ir::Side::All => report::Side::All,
        ir::Side::None => report::Side::None,
        ir::Side::Unspecified => report::Side::Unspecified,
    }
}

fn physical_side(value: pcb_ir::dialects::Side) -> report::Side {
    match value {
        pcb_ir::dialects::Side::Top => report::Side::Top,
        pcb_ir::dialects::Side::Bottom => report::Side::Bottom,
        pcb_ir::dialects::Side::Inner => report::Side::Internal,
        pcb_ir::dialects::Side::None => report::Side::None,
    }
}

fn component_mount(value: ir::Mount) -> report::ComponentMount {
    match value {
        ir::Mount::Smt => report::ComponentMount::Smt,
        ir::Mount::ThroughHole => report::ComponentMount::ThroughHole,
        ir::Mount::Embedded => report::ComponentMount::Embedded,
        ir::Mount::PressFit => report::ComponentMount::PressFit,
        ir::Mount::WireBonded => report::ComponentMount::WireBonded,
        ir::Mount::Glued => report::ComponentMount::Glued,
        ir::Mount::Clamped => report::ComponentMount::Clamped,
        ir::Mount::Socketed => report::ComponentMount::Socketed,
        ir::Mount::Formed => report::ComponentMount::Formed,
        ir::Mount::Other => report::ComponentMount::Other,
    }
}

fn bom_category(value: ir::BomCategory) -> report::BomCategory {
    match value {
        ir::BomCategory::Electrical => report::BomCategory::Electrical,
        ir::BomCategory::Programmable => report::BomCategory::Programmable,
        ir::BomCategory::Mechanical => report::BomCategory::Mechanical,
        ir::BomCategory::Material => report::BomCategory::Material,
        ir::BomCategory::Document => report::BomCategory::Document,
    }
}

fn affine(value: Affine2) -> [f64; 6] {
    [
        canonical_number(value.m00),
        canonical_number(value.m01),
        canonical_number(value.m10),
        canonical_number(value.m11),
        canonical_number(value.m02),
        canonical_number(value.m12),
    ]
}

fn point(value: IrPoint) -> report::Point {
    report::Point {
        x: canonical_number(value.x),
        y: canonical_number(value.y),
    }
}

fn contour(value: &ContourBuf) -> report::Contour {
    report::Contour {
        commands: value.cmds.iter().copied().map(path_command).collect(),
    }
}

fn path_command(value: PathCmd) -> report::PathCommand {
    match value.op {
        PathOp::MoveTo => report::PathCommand::MoveTo {
            x: canonical_number(value.p0.x),
            y: canonical_number(value.p0.y),
        },
        PathOp::LineTo => report::PathCommand::LineTo {
            x: canonical_number(value.p0.x),
            y: canonical_number(value.p0.y),
        },
        PathOp::ArcTo => report::PathCommand::ArcTo {
            x: canonical_number(value.p0.x),
            y: canonical_number(value.p0.y),
            center_x: canonical_number(value.p1.x),
            center_y: canonical_number(value.p1.y),
            clockwise: value.clockwise,
        },
        PathOp::CubicTo => report::PathCommand::CubicTo {
            control_1_x: canonical_number(value.p0.x),
            control_1_y: canonical_number(value.p0.y),
            control_2_x: canonical_number(value.p1.x),
            control_2_y: canonical_number(value.p1.y),
            x: canonical_number(value.p2.x),
            y: canonical_number(value.p2.y),
        },
        PathOp::Close => report::PathCommand::Close,
    }
}

fn source_transform(value: ir::Transform) -> report::SourceTransform {
    report::SourceTransform {
        x_offset_mm: canonical_number(value.x_offset),
        y_offset_mm: canonical_number(value.y_offset),
        rotation_degrees: canonical_number(value.rotation_degrees),
        mirror: value.mirror,
        face_up: value.face_up,
        scale: canonical_number(value.scale),
    }
}

fn canonical_number(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

#[derive(Default)]
struct IdAllocator {
    seen: HashMap<String, u32>,
}

impl IdAllocator {
    fn allocate(&mut self, prefix: &str, identity: &impl Serialize) -> String {
        let bytes = serde_json::to_vec(identity).expect("assembly report identity serializes");
        let digest = Sha256::digest(bytes);
        let base = format!("{prefix}-{}", hex::encode(&digest[..6]));
        let occurrence = self
            .seen
            .entry(base.clone())
            .and_modify(|count| *count += 1)
            .or_insert(1);
        if *occurrence == 1 {
            base
        } else {
            format!("{base}-{occurrence}")
        }
    }
}

#[cfg(test)]
mod tests;
