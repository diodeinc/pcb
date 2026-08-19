use std::collections::BTreeMap;

use crate::types::{
    BoardId, Contact, ContactId, Demand, DemandId, Ict, InterposerError, Kind, Problem,
};

/// Bundle contacts into demands. USB is one ordered pair per board.
/// Vtarget is one demand of 1..=2 pads per board. Unit kinds are 1:1.
pub fn bundle(contacts: Vec<Contact>) -> Result<Problem, InterposerError> {
    let mut problem = Problem::default();
    let mut by_board: BTreeMap<BoardId, Vec<ContactId>> = BTreeMap::new();
    for c in contacts {
        by_board.entry(c.board).or_default().push(c.id);
        problem.contacts.insert(c.id, c);
    }

    let mut next_demand = 0u32;
    let mut alloc = || {
        let id = DemandId(next_demand);
        next_demand += 1;
        id
    };

    for (board, ids) in by_board {
        let mut usb_dp = Vec::new();
        let mut usb_dm = Vec::new();
        let mut vtarget = Vec::new();
        let mut units: Vec<ContactId> = Vec::new();

        for id in ids {
            let ict = problem.contacts[&id].ict;
            match ict {
                Ict::UsbDp => usb_dp.push(id),
                Ict::UsbDm => usb_dm.push(id),
                Ict::Vtarget => vtarget.push(id),
                Ict::Swd => {}
                _ => units.push(id),
            }
        }

        if usb_dp.len() != usb_dm.len() || usb_dp.len() > 1 || usb_dm.len() > 1 {
            return Err(InterposerError::UnpairedUsb {
                board,
                dp: usb_dp.len(),
                dm: usb_dm.len(),
            });
        }
        if usb_dp.len() == 1 {
            let id = alloc();
            problem.demands.insert(
                id,
                Demand {
                    id,
                    kind: Kind::UsbHs,
                    board,
                    members: vec![usb_dp[0], usb_dm[0]],
                },
            );
        }

        if vtarget.len() > 2 {
            return Err(InterposerError::TooManyVtarget {
                board,
                count: vtarget.len(),
            });
        }
        if !vtarget.is_empty() {
            let id = alloc();
            problem.demands.insert(
                id,
                Demand {
                    id,
                    kind: Kind::Vtarget,
                    board,
                    members: vtarget,
                },
            );
        }

        for cid in units {
            let kind = problem.contacts[&cid]
                .ict
                .kind()
                .expect("unit ict maps to a kind");
            let id = alloc();
            problem.demands.insert(
                id,
                Demand {
                    id,
                    kind,
                    board,
                    members: vec![cid],
                },
            );
        }
    }

    Ok(problem)
}

pub fn next_contact_id(contacts: &[Contact]) -> ContactId {
    ContactId(
        contacts
            .iter()
            .map(|c| c.id.0)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{BoardId, Contact, ContactId, Ict};

    fn c(id: u32, board: u32, ict: Ict) -> Contact {
        Contact {
            id: ContactId(id),
            board: BoardId(board),
            xy: [0.0, 0.0],
            ict,
            path: format!("c{id}"),
            package: "TestPoint_Pad_D1.0mm".into(),
            side: "bottom".into(),
        }
    }

    #[test]
    fn unpaired_usb_fails_closed() {
        let err = bundle(vec![c(0, 0, Ict::UsbDp)]).unwrap_err();
        match err {
            InterposerError::UnpairedUsb { dp, dm, .. } => {
                assert_eq!(dp, 1);
                assert_eq!(dm, 0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn usb_pair_is_one_ordered_demand() {
        let p = bundle(vec![c(0, 0, Ict::UsbDp), c(1, 0, Ict::UsbDm)]).unwrap();
        assert_eq!(p.demands.len(), 1);
        let d = p.demands.values().next().unwrap();
        assert_eq!(d.kind, Kind::UsbHs);
        assert_eq!(d.members, vec![ContactId(0), ContactId(1)]);
    }
}
