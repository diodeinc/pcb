use ipc2581::Ipc2581;
use ipc2581::types::ecad::Step;

/// Resolve the IPC-2581 job step named by Content/StepRef, falling back to CadData order.
pub(crate) fn primary_step<'a>(ipc: &Ipc2581, steps: &'a [Step]) -> Option<&'a Step> {
    ipc.content()
        .step_refs
        .first()
        .and_then(|step_ref| steps.iter().find(|step| step.name == *step_ref))
        .or_else(|| steps.first())
}
