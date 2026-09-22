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
pub(super) struct LayoutParent<'a> {
    step: &'a Step,
    transform: Affine2,
    layout_step: u32,
    instance: Option<u32>,
}

pub fn extract_layout(ipc: &Ipc2581) -> Result<GeometryDocument> {
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let step =
        primary_step(ipc, &ecad.cad_data.steps).context("IPC-2581 ECAD section has no Step")?;
    let mut doc = GeometryDocument::new();
    // Only a panel or a board roots a layout.
    let kind = layout_step_kind(step);
    if matches!(kind, LayoutStepKind::Panel | LayoutStepKind::Board) {
        let root = ensure_layout_step_for_step(&mut doc, step);
        doc.layout.root_step = Some(root);
        if kind == LayoutStepKind::Panel {
            let parent = LayoutParent {
                step,
                transform: Affine2::identity(),
                layout_step: root,
                instance: None,
            };
            let steps = &ecad.cad_data.steps;
            append_layout_repeats(&mut doc, ipc, steps, parent, &mut vec![step.name])?;
        }
    }
    if ipc.resolve(step.name) == FAB_PANEL_STEP_NAME
        && let Some(root_step) = doc.layout.root_step
    {
        doc.layout.steps[root_step as usize].purpose = LayoutPurpose::FabricationPanel;
    }
    populate_ipc_specs(&mut doc, ipc, &ecad.cad_header.specs);
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

pub(super) fn append_layout_repeats(
    doc: &mut GeometryDocument,
    ipc: &Ipc2581,
    steps: &[Step],
    parent: LayoutParent<'_>,
    stack: &mut Vec<Symbol>,
) -> Result<()> {
    for repeat in &parent.step.step_repeats {
        let source_step = steps
            .iter()
            .find(|step| step.name == repeat.step_ref)
            .with_context(|| {
                format!(
                    "StepRepeat references unknown Step '{}'",
                    ipc.resolve(repeat.step_ref)
                )
            })?;

        if stack.contains(&source_step.name) {
            bail!(
                "StepRepeat cycle references Step '{}'",
                ipc.resolve(source_step.name)
            );
        }

        let child_layout_step = ensure_layout_step_for_step(doc, source_step);
        let layout_repeat = doc.layout.repeats.len();
        doc.layout.repeats.push(LayoutRepeat {
            parent_step: parent.layout_step,
            parent_instance: parent.instance,
            child_step: child_layout_step,
            source_step_ref: source_step.name,
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
                let layout_instance = doc.layout.instances.len() as u32;
                doc.layout.repeats[layout_repeat].instances.count += 1;
                doc.layout.instances.push(LayoutInstance {
                    repeat: layout_repeat as u32,
                    parent_instance: parent.instance,
                    child_step: child_layout_step,
                    source_step_ref: source_step.name,
                    transform,
                    repeat_index_x: ix,
                    repeat_index_y: iy,
                    bbox: BBox::empty(),
                });
                if layout_step_kind(source_step) == LayoutStepKind::Panel {
                    pending_panel_instances.push((source_step, transform, layout_instance));
                }
            }
        }

        for (source_step, transform, layout_instance) in pending_panel_instances {
            stack.push(source_step.name);
            append_layout_repeats(
                doc,
                ipc,
                steps,
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

    let (profiles, bbox) = append_step_profile(doc, step);
    doc.layout.steps.push(LayoutStep {
        source_step_ref: step.name,
        kind: layout_step_kind(step),
        purpose: LayoutPurpose::Product,
        profiles,
        bbox,
    });
    doc.layout.steps.len() as u32 - 1
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

/// Append `step`'s Profile, if it has a drawable one, and return its span in
/// `doc.profiles` with its bounds.
pub(super) fn append_step_profile(doc: &mut GeometryDocument, step: &Step) -> (Span, BBox) {
    let start = doc.profiles.len() as u32;
    let Some(profile) = &step.profile else {
        return (Span::new(start, 0), BBox::empty());
    };

    let mark = DocumentMark::of(doc);
    let outer_path = doc.push_path(Paint::None, [polygon_contour(&profile.polygon)]);
    let cutout_start = doc.profile_cutouts.len() as u32;
    for cutout in &profile.cutouts {
        let path = doc.push_path(Paint::None, [polygon_contour(cutout)]);
        doc.profile_cutouts.push(StepProfileCutout {
            path,
            bbox: doc.arena.paths[path as usize].bbox,
        });
    }
    if !mark.pushed_is_finite(doc) {
        mark.truncate(doc);
        doc.profile_cutouts.truncate(cutout_start as usize);
        doc.warn("Dropping a Step Profile because its geometry is not finite");
        return (Span::new(start, 0), BBox::empty());
    }
    let cutout_count = doc.profile_cutouts.len() as u32 - cutout_start;
    let bbox = doc.arena.paths[outer_path as usize].bbox;
    doc.profiles.push(StepProfile {
        outer_path,
        cutouts: Span::new(cutout_start, cutout_count),
        bbox,
    });
    (Span::new(start, 1), bbox)
}
