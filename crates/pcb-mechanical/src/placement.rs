use crate::datum::{FootprintDatums, LocalDatumPose};
use crate::idf::IdfPlacementClaim;
use pcb_sch::{
    ATTR_FOOTPRINT, AttributeValue, BoardPose, BoardSide, InstanceKind, InstanceRef, Schematic,
};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn resolve_mcad_positions(
    schematic: &Schematic,
    claims: &[IdfPlacementClaim],
    datums: &FootprintDatums,
) -> anyhow::Result<Vec<(InstanceRef, BoardPose)>> {
    let mut by_refdes = HashMap::<String, InstanceRef>::new();
    for (instance_ref, instance) in &schematic.instances {
        if instance.kind != InstanceKind::Component {
            continue;
        }
        if let Some(refdes) = &instance.reference_designator {
            by_refdes.insert(refdes.clone(), instance_ref.clone());
        }
    }

    let mut errors = Vec::new();
    let mut seen_refdes = HashSet::new();
    let mut resolved = Vec::new();

    // Ownership contract: IDF `MCAD` status means mechanical owns this component's position.
    for claim in claims.iter().filter(|claim| claim.mcad_owned) {
        if !seen_refdes.insert(claim.refdes.clone()) {
            errors.push(format!(
                "duplicate IDF placement for reference designator {}",
                claim.refdes
            ));
            continue;
        }

        let Some(instance_ref) = by_refdes.get(&claim.refdes).cloned() else {
            errors.push(format!(
                "IDF placement for {} has no matching Zener component instance",
                claim.refdes
            ));
            continue;
        };

        let footprint = schematic
            .instances
            .get(&instance_ref)
            .and_then(|instance| instance.attributes.get(ATTR_FOOTPRINT))
            .and_then(AttributeValue::string)
            .map(str::to_owned);
        let Some(footprint) = footprint else {
            errors.push(format!(
                "Zener component {} matched IDF placement but has no footprint",
                claim.refdes
            ));
            continue;
        };

        let Some(datum) = datums.lookup(&claim.package, &footprint) else {
            let extra = if datums.is_empty() {
                " (no footprint datums were found)"
            } else {
                ""
            };
            errors.push(format!(
                "no footprint datum for IDF package '{}' and footprint '{}'{}",
                claim.package, footprint, extra
            ));
            continue;
        };

        if let Some(expected_hash) = &datum.footprint_hash {
            match footprint_hash(schematic, &footprint) {
                Ok(Some(actual_hash)) if &actual_hash == expected_hash => {}
                Ok(Some(actual_hash)) => {
                    errors.push(format!(
                        "footprint hash mismatch for '{}': datum has {}, current file is {}",
                        footprint, expected_hash, actual_hash
                    ));
                    continue;
                }
                Ok(None) => {
                    errors.push(format!(
                        "datum for '{}' requires footprint hash {}, but the footprint path could not be resolved",
                        footprint, expected_hash
                    ));
                    continue;
                }
                Err(err) => {
                    errors.push(format!(
                        "failed to hash footprint '{}' for datum validation: {err}",
                        footprint
                    ));
                    continue;
                }
            }
        }

        let pose = resolve_footprint_pose(claim, datum.mechanical_origin_in_footprint);
        resolved.push((instance_ref, pose));
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "mechanical placement resolution failed:\n{}",
            errors.join("\n")
        );
    }

    Ok(resolved)
}

fn resolve_footprint_pose(claim: &IdfPlacementClaim, local: LocalDatumPose) -> BoardPose {
    // On the bottom side KiCad mirrors the footprint's local X axis (and negates
    // local angles), so flip the sign of the datum's X offset and rotation.
    let mirror = match claim.pose.side {
        BoardSide::Top => 1.0,
        BoardSide::Bottom => -1.0,
    };
    let footprint_rotation =
        normalize_degrees(claim.pose.rotation_deg - mirror * local.rotation_deg);
    let theta = footprint_rotation.to_radians();
    let local_x = mirror * local.x_mm;
    let dx = local_x * theta.cos() - local.y_mm * theta.sin();
    let dy = local_x * theta.sin() + local.y_mm * theta.cos();

    BoardPose {
        x_nm: mm_to_nm(claim.pose.x_mm - dx),
        y_nm: mm_to_nm(claim.pose.y_mm - dy),
        rotation_deg: footprint_rotation,
        side: claim.pose.side,
    }
}

fn mm_to_nm(mm: f64) -> i64 {
    (mm * 1_000_000.0).round() as i64
}

fn normalize_degrees(deg: f64) -> f64 {
    deg.rem_euclid(360.0)
}

fn footprint_hash(schematic: &Schematic, footprint: &str) -> anyhow::Result<Option<String>> {
    let path = if footprint.starts_with(pcb_sch::PACKAGE_URI_PREFIX) {
        schematic.resolve_package_uri(footprint).ok()
    } else {
        Some(PathBuf::from(footprint))
    };
    let Some(path) = path else {
        return Ok(None);
    };
    if !Path::new(&path).exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    Ok(Some(format!("blake3:{}", blake3::hash(&bytes).to_hex())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datum::{FootprintDatum, FootprintDatums, LocalDatumPose};
    use crate::idf::MechanicalPose;
    use pcb_sch::{Instance, ModuleRef};

    #[test]
    fn resolves_claim_to_component_placement() {
        let mut schematic = Schematic::new();
        let module = ModuleRef::new("/board.zen", "<root>");
        let instance_ref = InstanceRef::new(module.clone(), vec!["J".to_owned()]);
        let mut instance = Instance::component(module);
        instance.reference_designator = Some("J1".to_owned());
        instance.add_attribute("footprint", "package://pkg/fp.kicad_mod".to_owned());
        schematic.instances.insert(instance_ref.clone(), instance);

        let datums = FootprintDatums::from_entries_for_test(vec![FootprintDatum {
            idf_package: "USB".to_owned(),
            footprint: "package://pkg/fp.kicad_mod".to_owned(),
            footprint_hash: None,
            mechanical_origin_in_footprint: LocalDatumPose {
                x_mm: 1.0,
                y_mm: 0.0,
                rotation_deg: 0.0,
            },
        }]);

        let claim = IdfPlacementClaim {
            refdes: "J1".to_owned(),
            package: "USB".to_owned(),
            part_number: None,
            pose: MechanicalPose {
                x_mm: 10.0,
                y_mm: 20.0,
                rotation_deg: 0.0,
                side: BoardSide::Top,
            },
            mcad_owned: true,
        };

        let positions = resolve_mcad_positions(&schematic, &[claim], &datums).unwrap();
        let (resolved_ref, pose) = positions.into_iter().next().unwrap();
        assert_eq!(resolved_ref, instance_ref);
        assert_eq!(pose.x_nm, 9_000_000);
        assert_eq!(pose.y_nm, 20_000_000);
        assert_eq!(pose.side, BoardSide::Top);
    }

    #[test]
    fn bottom_side_mirrors_datum_x_offset() {
        let claim = IdfPlacementClaim {
            refdes: "J1".to_owned(),
            package: "USB".to_owned(),
            part_number: None,
            pose: MechanicalPose {
                x_mm: 10.0,
                y_mm: 20.0,
                rotation_deg: 0.0,
                side: BoardSide::Bottom,
            },
            mcad_owned: true,
        };
        let local = LocalDatumPose {
            x_mm: 1.0,
            y_mm: 0.0,
            rotation_deg: 0.0,
        };

        let pose = resolve_footprint_pose(&claim, local);
        // The same datum on the top resolves to x = 9mm; the bottom-side mirror
        // flips the X offset, landing the footprint origin at x = 11mm.
        assert_eq!(pose.x_nm, 11_000_000);
        assert_eq!(pose.y_nm, 20_000_000);
        assert_eq!(pose.side, BoardSide::Bottom);
    }
}
