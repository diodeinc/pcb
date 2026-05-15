pub mod dxf;
mod extract;

use anyhow::{Context, Result};
use ipc2581::Ipc2581;
use ipc2581::types::ecad::Step;

pub use extract::extract_layer;

pub(crate) fn primary_step<'a>(ipc: &Ipc2581, steps: &'a [Step]) -> Result<&'a Step> {
    if let Some(step_ref) = ipc.content().step_refs.first()
        && let Some(step) = steps.iter().find(|step| step.name == *step_ref)
    {
        return Ok(step);
    }

    steps.first().context("IPC-2581 ECAD section has no Step")
}
