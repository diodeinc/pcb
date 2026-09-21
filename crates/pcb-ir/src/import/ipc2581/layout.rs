//! The layout graph: steps, their profiles, and StepRepeat expansion.

use super::*;

pub const FAB_PANEL_STEP_NAME: &str = "fab_panel";

/// The Step a file is about: the first the Content references, else the
/// first declared.
pub fn primary_step<'a>(ipc: &Ipc2581, steps: &'a [Step]) -> Option<&'a Step> {
    ipc.content()
        .step_refs
        .first()
        .and_then(|step_ref| steps.iter().find(|step| step.name == *step_ref))
        .or_else(|| steps.first())
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ProfileRange {
    start: u32,
    count: u32,
    bbox: BBox,
}

pub(super) struct LayoutBuildContext<'a> {
    ipc: &'a Ipc2581,
    steps: &'a [Step],
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LayoutParent<'a> {
    step: &'a Step,
    transform: Affine2,
    layout_step: u32,
    instance: Option<u32>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LayoutInstanceSpec {
    repeat: u32,
    parent_instance: Option<u32>,
    child_step: u32,
    source_step_ref: Symbol,
    parent_step_ref: Symbol,
    transform: Affine2,
    repeat_index_x: u32,
    repeat_index_y: u32,
    repeat_count_x: u32,
    repeat_count_y: u32,
    repeat_pitch_x: f64,
    repeat_pitch_y: f64,
}

pub fn extract_layout(ipc: &Ipc2581) -> Result<GeometryDocument> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let step =
        primary_step(ipc, &ecad.cad_data.steps).context("IPC-2581 ECAD section has no Step")?;
    let mut doc = GeometryDocument::new();
    append_layout_geometry(&mut doc, ipc, &ecad.cad_data.steps, step)?;
    if ipc.resolve(step.name) == FAB_PANEL_STEP_NAME
        && let Some(root_step) = doc.layout.root_step
    {
        doc.layout.steps[root_step as usize].purpose = LayoutPurpose::FabricationPanel;
    }
    populate_ipc_specs(&mut doc, ipc);
    crate::dialects::ipc::process::normalize_bounds(&mut doc);
    Ok(doc)
}

/// Placement of one StepRepeat instance in its parent step, given the
/// repeated child step's Datum.
///
/// A Step's Datum is its point of origin, and x/y plus the grid pitch say
/// where that point goes (IPC-2581C 8.2.3.3 and 8.2.3.5, as in ODB++
/// step-and-repeat): the child turns and mirrors about its datum.
pub fn step_repeat_transform(
    child_datum: Option<Datum>,
    repeat: &StepRepeat,
    ix: u32,
    iy: u32,
) -> Affine2 {
    let datum = child_datum.unwrap_or(Datum { x: 0.0, y: 0.0 });
    ipc_placement(
        Point::new(
            repeat.x + ix as f64 * repeat.dx,
            repeat.y + iy as f64 * repeat.dy,
        ),
        Some(Xform {
            rotation: repeat.angle,
            mirror: repeat.mirror,
            x_offset: -datum.x,
            y_offset: -datum.y,
            ..Xform::default()
        }),
    )
    .transform
}

pub(super) fn append_layout_geometry(
    doc: &mut GeometryDocument,
    ipc: &Ipc2581,
    steps: &[Step],
    primary_step: &Step,
) -> Result<()> {
    if is_panel_step(primary_step) {
        append_panel_geometry(doc, ipc, steps, primary_step)
    } else if is_board_step(primary_step) {
        doc.layout.root_step = Some(ensure_layout_step_for_step(doc, primary_step));
        Ok(())
    } else {
        Ok(())
    }
}

pub(super) fn append_panel_geometry(
    doc: &mut GeometryDocument,
    ipc: &Ipc2581,
    steps: &[Step],
    panel_step: &Step,
) -> Result<()> {
    let panel_profiles = append_step_profile(doc, panel_step);
    let root_layout_step = push_or_update_layout_step(doc, panel_step, panel_profiles);
    doc.layout.root_step = Some(root_layout_step);
    let context = LayoutBuildContext { ipc, steps };
    let parent = LayoutParent {
        step: panel_step,
        transform: Affine2::identity(),
        layout_step: root_layout_step,
        instance: None,
    };
    let mut stack = vec![panel_step.name];
    append_layout_repeats(doc, &context, parent, &mut stack)?;

    Ok(())
}

pub(super) fn append_layout_repeats(
    doc: &mut GeometryDocument,
    context: &LayoutBuildContext<'_>,
    parent: LayoutParent<'_>,
    stack: &mut Vec<Symbol>,
) -> Result<()> {
    for repeat in &parent.step.step_repeats {
        let source_step = context
            .steps
            .iter()
            .find(|step| step.name == repeat.step_ref)
            .with_context(|| {
                format!(
                    "StepRepeat references unknown Step '{}'",
                    context.ipc.resolve(repeat.step_ref)
                )
            })?;

        if stack.contains(&source_step.name) {
            bail!(
                "StepRepeat cycle references Step '{}'",
                context.ipc.resolve(source_step.name)
            );
        }

        let child_layout_step = ensure_layout_step_for_step(doc, source_step);
        let layout_repeat = push_layout_repeat(
            doc,
            parent.layout_step,
            parent.instance,
            child_layout_step,
            source_step.name,
            repeat,
        );

        // Bound expansion where it happens, including nested repeats. Empty
        // repeats keep their metadata without iterating a potentially huge ny.
        if repeat.nx == 0 || repeat.ny == 0 {
            continue;
        }
        if stack.len() >= 64 {
            bail!("IPC layout nesting exceeds the limit of 64 Steps");
        }
        if (repeat.nx as usize)
            .checked_mul(repeat.ny as usize)
            .and_then(|count| doc.layout.instances.len().checked_add(count))
            .is_none_or(|count| count > 100_000)
        {
            bail!("IPC layout exceeds the limit of 100000 Step instances");
        }

        let mut pending_panel_instances = Vec::new();
        for iy in 0..repeat.ny {
            for ix in 0..repeat.nx {
                let transform = parent.transform.concat(step_repeat_transform(
                    source_step.datum,
                    repeat,
                    ix,
                    iy,
                ));
                let layout_instance = push_layout_instance(
                    doc,
                    LayoutInstanceSpec {
                        repeat: layout_repeat,
                        parent_instance: parent.instance,
                        child_step: child_layout_step,
                        source_step_ref: source_step.name,
                        parent_step_ref: parent.step.name,
                        transform,
                        repeat_index_x: ix,
                        repeat_index_y: iy,
                        repeat_count_x: repeat.nx,
                        repeat_count_y: repeat.ny,
                        repeat_pitch_x: repeat.dx,
                        repeat_pitch_y: repeat.dy,
                    },
                );
                if is_panel_step(source_step) {
                    pending_panel_instances.push((source_step, transform, layout_instance));
                }
            }
        }

        for (source_step, transform, layout_instance) in pending_panel_instances {
            stack.push(source_step.name);
            append_layout_repeats(
                doc,
                context,
                LayoutParent {
                    step: source_step,
                    transform,
                    layout_step: child_layout_step,
                    instance: Some(layout_instance),
                },
                stack,
            )?;
            stack.pop();
        }
    }

    Ok(())
}

pub(super) fn ensure_layout_step_for_step(doc: &mut GeometryDocument, step: &Step) -> u32 {
    if let Some(index) = doc
        .layout
        .steps
        .iter()
        .position(|layout_step| layout_step.source_step_ref == step.name)
    {
        return index as u32;
    }

    let profiles = append_step_profile(doc, step);
    push_or_update_layout_step(doc, step, profiles)
}

pub(super) fn push_or_update_layout_step(
    doc: &mut GeometryDocument,
    step: &Step,
    profiles: ProfileRange,
) -> u32 {
    if let Some(index) = doc
        .layout
        .steps
        .iter()
        .position(|layout_step| layout_step.source_step_ref == step.name)
    {
        let layout_step = &mut doc.layout.steps[index];
        if layout_step.profiles.is_empty() && profiles.count > 0 {
            layout_step.profiles = Span::new(profiles.start, profiles.count);
            layout_step.bbox = profiles.bbox;
        }
        return index as u32;
    }

    let index = doc.layout.steps.len() as u32;
    doc.layout.steps.push(LayoutStep {
        source_step_ref: step.name,
        kind: layout_step_kind(step),
        purpose: LayoutPurpose::Product,
        profiles: Span::new(profiles.start, profiles.count),
        bbox: profiles.bbox,
    });
    index
}

pub(super) fn push_layout_repeat(
    doc: &mut GeometryDocument,
    parent_step: u32,
    parent_instance: Option<u32>,
    child_step: u32,
    source_step_ref: Symbol,
    repeat: &StepRepeat,
) -> u32 {
    let repeat_index = doc.layout.repeats.len() as u32;

    doc.layout.repeats.push(LayoutRepeat {
        parent_step,
        parent_instance,
        child_step,
        source_step_ref,
        x: repeat.x,
        y: repeat.y,
        nx: repeat.nx,
        ny: repeat.ny,
        dx: repeat.dx,
        dy: repeat.dy,
        angle: repeat.angle,
        mirror: repeat.mirror,
        instances: Span::new(doc.layout.instances.len() as u32, 0),
        bbox: BBox::empty(),
    });
    repeat_index
}

pub(super) fn push_layout_instance(doc: &mut GeometryDocument, spec: LayoutInstanceSpec) -> u32 {
    let instance_index = doc.layout.instances.len() as u32;
    let repeat_record = &mut doc.layout.repeats[spec.repeat as usize];
    if repeat_record.instances.is_empty() {
        repeat_record.instances.start = instance_index;
    }
    repeat_record.instances.count += 1;

    doc.layout.instances.push(LayoutInstance {
        repeat: spec.repeat,
        parent_instance: spec.parent_instance,
        child_step: spec.child_step,
        source_step_ref: spec.source_step_ref,
        parent_step_ref: spec.parent_step_ref,
        transform: spec.transform,
        repeat_index_x: spec.repeat_index_x,
        repeat_index_y: spec.repeat_index_y,
        repeat_count_x: spec.repeat_count_x,
        repeat_count_y: spec.repeat_count_y,
        repeat_pitch_x: spec.repeat_pitch_x,
        repeat_pitch_y: spec.repeat_pitch_y,
        bbox: BBox::empty(),
    });
    instance_index
}

pub(super) fn layout_step_kind(step: &Step) -> LayoutStepKind {
    match step.step_type {
        Some(StepType::Board) => LayoutStepKind::Board,
        Some(StepType::Pallet) => LayoutStepKind::Panel,
        Some(StepType::Ic) => LayoutStepKind::Ic,
        None if !step.step_repeats.is_empty() => LayoutStepKind::Panel,
        None => LayoutStepKind::Board,
    }
}

pub fn is_panel_step(step: &Step) -> bool {
    is_panel(step.step_type, &step.step_repeats)
}

pub(super) fn is_panel(step_type: Option<StepType>, step_repeats: &[StepRepeat]) -> bool {
    matches!(step_type, Some(StepType::Pallet)) || (step_type.is_none() && !step_repeats.is_empty())
}

pub(super) fn is_board_step(step: &Step) -> bool {
    matches!(step.step_type, Some(StepType::Board))
        || (step.step_type.is_none() && step.step_repeats.is_empty())
}

pub(super) fn append_step_profile(doc: &mut GeometryDocument, step: &Step) -> ProfileRange {
    let start = doc.profiles.len() as u32;
    let Some(profile) = &step.profile else {
        return ProfileRange {
            start,
            count: 0,
            bbox: BBox::empty(),
        };
    };

    let mark = DocumentMark::of(doc);
    let outer_path = push_profile_polygon(doc, &profile.polygon);
    let cutout_start = doc.profile_cutouts.len() as u32;
    for cutout in &profile.cutouts {
        let path = push_profile_polygon(doc, cutout);
        doc.profile_cutouts.push(StepProfileCutout {
            path,
            bbox: doc.arena.paths[path as usize].bbox,
        });
    }
    if !mark.pushed_is_finite(doc) {
        mark.truncate(doc);
        doc.profile_cutouts.truncate(cutout_start as usize);
        doc.warn("Dropping a Step Profile because its geometry is not finite");
        return ProfileRange {
            start,
            count: 0,
            bbox: BBox::empty(),
        };
    }
    let cutout_count = doc.profile_cutouts.len() as u32 - cutout_start;
    let bbox = doc.arena.paths[outer_path as usize].bbox;
    doc.profiles.push(StepProfile {
        outer_path,
        cutouts: Span::new(cutout_start, cutout_count),
        bbox,
    });
    ProfileRange {
        start,
        count: doc.profiles.len() as u32 - start,
        bbox,
    }
}

pub(super) fn push_profile_polygon(
    doc: &mut GeometryDocument,
    polygon: &ipc2581::types::Polygon,
) -> u32 {
    let contour = polygon_contour(polygon);
    doc.push_path(Paint::None, [contour])
}
