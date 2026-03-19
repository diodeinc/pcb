use crate::bom::ComponentKey;

use super::search::{AvailabilityKey, AvailabilityRequest, PricingResponse, PricingResult};
use std::collections::HashMap;
use std::time::{Duration, Instant};

enum AvailabilityState {
    Pending { started_at: Instant },
    Ready(Box<pcb_sch::bom::Availability>),
    Empty,
}

pub(super) struct AvailabilityStore {
    by_key: HashMap<AvailabilityKey, AvailabilityState>,
}

impl AvailabilityStore {
    pub(super) fn new() -> Self {
        Self {
            by_key: HashMap::new(),
        }
    }

    pub(super) fn clear_pending(&mut self) {
        self.by_key
            .retain(|_, state| !matches!(state, AvailabilityState::Pending { .. }));
    }

    pub(super) fn queue_requests(
        &mut self,
        requests: impl IntoIterator<Item = AvailabilityRequest>,
        started_at: Instant,
    ) -> Vec<AvailabilityRequest> {
        let mut queued = Vec::new();
        for request in requests {
            if request.lookups.is_empty() || self.by_key.contains_key(&request.key) {
                continue;
            }
            self.by_key.insert(
                request.key.clone(),
                AvailabilityState::Pending { started_at },
            );
            queued.push(request);
        }
        queued
    }

    pub(super) fn apply_response(&mut self, response: PricingResponse) {
        for (key, result) in response {
            match result {
                PricingResult::Ready(availability) => {
                    self.by_key
                        .insert(key, AvailabilityState::Ready(availability));
                }
                PricingResult::Empty => {
                    self.by_key.insert(key, AvailabilityState::Empty);
                }
                PricingResult::Failed => {
                    self.by_key.remove(&key);
                }
            }
        }
    }

    pub(super) fn component(
        &self,
        key: &ComponentKey,
    ) -> (Option<&pcb_sch::bom::Availability>, bool) {
        self.lookup(&AvailabilityKey::Component(key.clone()))
    }

    pub(super) fn kicad_symbol(
        &self,
        symbol_id: i64,
    ) -> (Option<&pcb_sch::bom::Availability>, bool) {
        self.lookup(&AvailabilityKey::KicadSymbol(symbol_id))
    }

    fn lookup(&self, key: &AvailabilityKey) -> (Option<&pcb_sch::bom::Availability>, bool) {
        const LOADING_DELAY_MS: u64 = 150;

        match self.by_key.get(key) {
            Some(AvailabilityState::Pending { started_at }) => (
                None,
                started_at.elapsed() > Duration::from_millis(LOADING_DELAY_MS),
            ),
            Some(AvailabilityState::Ready(availability)) => (Some(availability.as_ref()), false),
            Some(AvailabilityState::Empty) | None => (None, false),
        }
    }
}

pub(super) fn selected_first_indices(len: usize, selected: Option<usize>) -> Vec<usize> {
    let mut indices = Vec::with_capacity(len);

    if let Some(selected) = selected.filter(|&idx| idx < len) {
        indices.push(selected);
    }

    for idx in 0..len {
        if Some(idx) != selected {
            indices.push(idx);
        }
    }

    indices
}
