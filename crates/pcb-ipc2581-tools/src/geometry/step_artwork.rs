//! One layer of the IPC Step graph as artwork, every Step lowered once.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use ipc2581::Symbol;
use ipc2581::types::{LayerFunction, Step, StepRepeat};
use pcb_ir::dialects::artwork::{
    Document, Geometry, GridRepeat, Layer, Object, PaintStage, normalize_bounds,
};
use pcb_ir::geom::{Point, Polarity};
use pcb_ir::import::ipc2581::{ImportedDesign, LayerId, step_repeat_transform};

type GeometryDocument = pcb_ir::dialects::ipc::Document<Symbol, LayerFunction>;

/// The Step a scope draws from: the design's primary Step, or the board
/// definition it places.
pub fn root_step(imported: &ImportedDesign, board: bool) -> Result<&Step> {
    let source = if board {
        pcb_ir::dialects::ipc::layout_steps_by_kind(
            &imported.geometry,
            pcb_ir::dialects::ipc::LayoutStepKind::Board,
        )
        .next()
        .map(|(_, step)| step.source_step_ref)
        .context("IPC-2581 primary step does not reference a board step")?
    } else {
        pcb_ir::dialects::ipc::root_step(&imported.geometry)
            .map(|(_, step)| step.source_step_ref)
            .context("IPC-2581 primary step has no canonical layout root")?
    };
    find_step(imported, source)
}

fn find_step(imported: &ImportedDesign, name: Symbol) -> Result<&Step> {
    imported
        .steps
        .iter()
        .find(|step| step.name == name)
        .with_context(|| format!("unknown Step '{}'", imported.resolve(name)))
}

/// Lower `layer` of the Step graph under `root` to artwork.
///
/// `lower_step` normalizes one Step's local layer document and lowers it into
/// the artwork's tables, returning the Step's own objects. The root's objects
/// land directly on the layer and every repeated child Step becomes blocks
/// placed by instances, so a board repeated into an assembly panel and that
/// panel repeated into a fabrication panel stays a hierarchy — and a Step
/// without repeats lowers to plain flat artwork.
///
/// Blocks are pure in paint stage. A Step contributes one block of what it
/// paints and one of its final cutouts, each placed by instances of that
/// stage, so the layer orders cutouts after all paint exactly as its flat
/// expansion would.
pub fn step_graph_artwork<LayerMeta, ObjectMeta: Default>(
    imported: &ImportedDesign,
    layer: LayerId,
    root: &Step,
    header: Layer<LayerMeta>,
    mut lower_step: impl FnMut(
        &Step,
        GeometryDocument,
        &mut Document<LayerMeta, ObjectMeta>,
    ) -> Result<Vec<Object<ObjectMeta>>>,
) -> Result<Document<LayerMeta, ObjectMeta>> {
    let mut graph = StepGraph {
        imported,
        layer,
        artwork: Document::new(),
        blocks: HashMap::from([(root.name, None)]),
        lower_step: &mut lower_step,
    };
    let artwork_layer = graph.artwork.push_layer(header);
    for object in graph.step_objects(root)?.into_iter().flatten() {
        graph.artwork.push_object(artwork_layer, object);
    }
    let mut artwork = graph.artwork;
    normalize_bounds(&mut artwork);
    artwork
        .validate()
        .map_err(|error| anyhow::anyhow!("invalid Step graph artwork: {error}"))?;
    Ok(artwork)
}

/// A Step's objects by stage: what it paints, then its final cutouts.
type Staged<T> = [T; 2];

type LowerStep<'a, LayerMeta, ObjectMeta> = dyn FnMut(
        &Step,
        GeometryDocument,
        &mut Document<LayerMeta, ObjectMeta>,
    ) -> Result<Vec<Object<ObjectMeta>>>
    + 'a;

struct StepGraph<'a, LayerMeta, ObjectMeta> {
    imported: &'a ImportedDesign,
    layer: LayerId,
    artwork: Document<LayerMeta, ObjectMeta>,
    /// Each Step's blocks by stage, `None` for a stage it leaves empty. A
    /// Step still being built maps to `None`, which is how a cycle shows.
    blocks: HashMap<Symbol, Option<Staged<Option<u32>>>>,
    lower_step: &'a mut LowerStep<'a, LayerMeta, ObjectMeta>,
}

impl<LayerMeta, ObjectMeta: Default> StepGraph<'_, LayerMeta, ObjectMeta> {
    fn step_blocks(&mut self, step: &Step) -> Result<Staged<Option<u32>>> {
        match self.blocks.get(&step.name) {
            Some(Some(blocks)) => return Ok(*blocks),
            Some(None) => bail!(
                "StepRepeat cycle references Step '{}'",
                self.imported.resolve(step.name)
            ),
            None => {}
        }
        self.blocks.insert(step.name, None);
        let blocks = self.step_objects(step)?.map(|objects| {
            (!objects.is_empty()).then(|| {
                let block = self.artwork.push_block();
                for object in objects {
                    self.artwork.push_block_object(block, object);
                }
                block
            })
        });
        self.blocks.insert(step.name, Some(blocks));
        Ok(blocks)
    }

    fn step_objects(&mut self, step: &Step) -> Result<Staged<Vec<Object<ObjectMeta>>>> {
        // Children first: a block may only reference earlier blocks.
        let children = step
            .step_repeats
            .iter()
            .map(|repeat| {
                let child = find_step(self.imported, repeat.step_ref)?;
                Ok((self.step_blocks(child)?, repeat))
            })
            .collect::<Result<Vec<_>>>()?;

        let step_id = self
            .imported
            .step_id(step.name)
            .context("source Step is missing from the canonical layout graph")?;
        let local = self
            .imported
            .materialize_step_layer(step_id, self.layer)
            .with_context(|| {
                format!(
                    "failed to materialize IPC-2581 Step '{}'",
                    self.imported.resolve(step.name)
                )
            })?;
        let (cutouts, painted): (Vec<_>, Vec<_>) =
            (self.lower_step)(step, local, &mut self.artwork)?
                .into_iter()
                .partition(|object| object.order.stage == PaintStage::FinalCutout);
        let mut staged = [painted, cutouts];
        for (blocks, repeat) in children {
            if repeat.nx == 0 || repeat.ny == 0 {
                continue;
            }
            for (stage, block) in blocks.into_iter().enumerate() {
                let Some(block) = block else { continue };
                let mut object = Object::new(Polarity::Dark, placement(block, repeat));
                if stage == 1 {
                    object.order.stage = PaintStage::FinalCutout;
                }
                staged[stage].push(object);
            }
        }
        Ok(staged)
    }
}

fn placement(block: u32, repeat: &StepRepeat) -> Geometry {
    let transform = step_repeat_transform(repeat, 0, 0);
    if repeat.nx > 1 || repeat.ny > 1 {
        Geometry::GridInstance {
            block,
            transform,
            repeat: GridRepeat {
                x_count: repeat.nx,
                y_count: repeat.ny,
                x_step: Point::new(repeat.dx, 0.0),
                y_step: Point::new(0.0, repeat.dy),
            },
        }
    } else {
        Geometry::Instance { block, transform }
    }
}
