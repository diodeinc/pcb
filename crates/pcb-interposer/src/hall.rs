use crate::types::{InterposerError, Kind, Problem};

/// Counting-only Hall check per kind.
pub fn hall(problem: &Problem) -> Result<(), InterposerError> {
    for kind in Kind::ALL {
        let demands: Vec<_> = problem.demands_of(kind);
        let slots: Vec<_> = problem.slots_of(kind);
        if demands.is_empty() {
            continue;
        }
        let need: usize = demands
            .iter()
            .map(|id| problem.demands[id].members.len())
            .sum();
        let have: usize = slots
            .iter()
            .map(|id| problem.slots[id].shape.capacity())
            .sum();
        let too_many_members = demands.iter().any(|id| {
            let d = &problem.demands[id];
            slots
                .iter()
                .all(|sid| d.members.len() > problem.slots[sid].shape.capacity())
        });
        if demands.len() > slots.len() || need > have || too_many_members {
            return Err(InterposerError::Hall {
                kind,
                demands: demands.len(),
                need,
                slots: slots.len(),
                have,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::bundle;
    use crate::pattern::{PITCH_254, PatternKind, attach_pattern, generate_pattern};
    use crate::types::{BoardId, Contact, ContactId, Ict};

    fn c(id: u32, board: u32, ict: Ict) -> Contact {
        Contact {
            id: ContactId(id),
            board: BoardId(board),
            xy: [id as f64, 0.0],
            ict,
            path: format!("c{id}"),
            package: "TestPoint_Pad_D1.0mm".into(),
            side: "bottom".into(),
        }
    }

    #[test]
    fn nine_usb_pairs_fail_hall() {
        let mut cs = Vec::new();
        let mut id = 0;
        for b in 0..9u32 {
            cs.push(c(id, b, Ict::UsbDp));
            id += 1;
            cs.push(c(id, b, Ict::UsbDm));
            id += 1;
        }
        let mut p = bundle(cs).unwrap();
        attach_pattern(&mut p, &generate_pattern(PatternKind::S1, PITCH_254));
        let err = hall(&p).unwrap_err();
        match err {
            InterposerError::Hall { kind, .. } => assert_eq!(kind, Kind::UsbHs),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn three_vtarget_on_one_board_fails_closed() {
        let cs = vec![
            c(0, 0, Ict::Vtarget),
            c(1, 0, Ict::Vtarget),
            c(2, 0, Ict::Vtarget),
        ];
        let err = bundle(cs).unwrap_err();
        match err {
            InterposerError::TooManyVtarget { count, .. } => assert_eq!(count, 3),
            other => panic!("{other:?}"),
        }
    }
}
