use crate::geom::Resolution;
use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result, bail};
use ipc2581::types::{
    ConcentricShape, FillProperty, HoleShape as IpcHoleShape, LayerFunction, LineEnd, LineProperty,
    PadUse, PlatingStatus, Polarity, PolyStep, SlotShape, StandardPrimitive, UserPrimitive,
    UserShapeType, Xform,
    ecad::{Datum, FeatureShape, Layer, SetFeature, Step, StepRepeat, StepType},
};
use ipc2581::{Interner, Ipc2581, Symbol};

use crate::dialects::ipc::*;
use crate::geom::Polarity as GeometryPolarity;
use crate::geom::*;
use crate::import::physical::{feature_definitely_spans_layer, physical_stackup_layers};

pub use crate::dialects::assembly::{
    BomReferenceId, ComponentDefinitionId, ComponentOccurrenceId, LayoutOccurrenceId,
    PackageDefinitionId, Population as PopulationState,
};

pub type GeometryDocument = crate::dialects::ipc::Document;
type GeometryLayer = crate::dialects::ipc::Layer;
type GeometryFeature = crate::dialects::ipc::Feature;

mod append;
mod assembly;
mod copper_balance;
mod design;
mod features;
mod intent;
mod layout;
mod primitives;
mod specs;
#[cfg(test)]
mod tests;

use self::{append::*, primitives::*, specs::*};
pub use self::{copper_balance::*, design::*, features::*, intent::*, layout::*};
