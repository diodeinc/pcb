//! Minimum routed slot width.
//!
//! A routed slot's width is fixed at extraction ([`Slot::width`]): the
//! stated primitive width when the source gives one — exact, and verified
//! against the materialized outline — and otherwise the outline's narrowest
//! local width, the separation of its two facing walls. The check is the
//! pointwise predicate `wₛ ≥ W_min` for every slot `s`.

use crate::commands::dfm::design::{Design, Slot};
use crate::commands::dfm::pdk::SlotPlating;
use crate::commands::dfm::report::{Evidence, EvidenceDisplay, MeasurementKind};
use pcb_ir::geom::BBox;
use pcb_ir::geom::dfm::thin_features;

use super::{Evaluation, Measured, MeasuredSite, slot_subject, thin_regions, violates};

pub(super) fn evaluate(limit_mm: f64, plating: SlotPlating, design: &Design) -> Evaluation {
    let slots = design
        .slots
        .iter()
        .filter(|slot| super::slot_matches(slot.plating, plating))
        .collect::<Vec<_>>();
    let measured = slots
        .iter()
        .map(|&slot: &&Slot| {
            let subject = slot_subject(design, slot, "offender");
            let sites = if !violates(&slot.width, limit_mm) {
                Vec::new()
            } else if let Some(nominal) = slot.nominal_width_mm {
                let center = slot.width_disk.center;
                let radial = slot.width_disk.width.first - center;
                let direction = radial / radial.length();
                let first = center - direction * (nominal / 2.0);
                let second = center + direction * (nominal / 2.0);
                let mut site = MeasuredSite::new(
                    slot.width,
                    slot.bbox.union(BBox::from_point(slot.width_disk.center).expand(limit_mm / 2.0)),
                    vec![slot.layer.clone()],
                    vec![
                        Evidence {
                            display: Some(EvidenceDisplay::Path {
                                paths: slot.native_outline.iter().map(|contour| {
                                    pcb_ir::render::svg_path_data(std::slice::from_ref(contour))
                                }).collect(),
                                fill_rule: "evenodd",
                            }),
                            ..Evidence::region("routed_slot", &slot.outline)
                        },
                        Evidence::circle("nominal_width_disk", center, nominal),
                        Evidence::segment("nominal_width_dimension", first, second),
                        Evidence::circle("required_width_disk", slot.width_disk.center, limit_mm),
                    ],
                    MeasurementKind::NominalWidth,
                );
                site.note = Some("The slot primitive declares this width; its materialized outline was checked for agreement.".to_owned());
                vec![site]
            } else {
                thin_features(&slot.outline, limit_mm).iter()
                    .flat_map(|piece| thin_regions::piece_sites(piece, limit_mm, &slot.layer)).collect()
            };
            Measured {
            distance: slot.width,
            bbox: slot.bbox,
            layers: vec![slot.layer.clone()],
            subjects: vec![subject],
            evidence: vec![Evidence::bounds("routed_slot", slot.bbox)],
            sites,
        }})
        .collect();
    Evaluation {
        checked: slots.len(),
        measured,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::dfm::{pdk::Pdk, rules};
    use crate::ipc2581::Ipc2581;
    use pcb_ir::dialects::ipc::ArtworkScope;

    #[test]
    fn nominal_slot_display_retains_native_curves_and_the_checked_outline() {
        let ipc = Ipc2581::parse(r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
          <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/><LayerRef name="ROUT"/></Content>
          <Ecad><CadHeader units="MILLIMETER"/><CadData>
            <Layer name="ROUT" layerFunction="ROUT" side="ALL" polarity="POSITIVE"/>
            <Step name="board" type="BOARD"><LayerFeature layerRef="ROUT"><Set>
              <SlotCavity name="S1" platingStatus="PLATED" plusTol="0" minusTol="0">
                <Location x="10" y="20"/><Oval width="1.8" height="0.6"/>
              </SlotCavity>
            </Set></LayerFeature></Step>
          </CadData></Ecad>
        </IPC-2581>"#).unwrap();
        let pdk = Pdk::parse(
            r#"schema_version = 2
          default_profile = "test"
          [pdk]
          id = "test"
          name = "Test"
          revision = "1"
          [profiles.test]
          name = "Test"
          [[rules.drilling.slot_width]]
          id = "slot-width"
          select = { plating = "plated" }
          limit = { minimum = "0.8 mm" }
        "#,
        )
        .unwrap();
        let rules = rules::lower(&pdk, None).unwrap();
        let imported = pcb_ir::import::ipc2581::import_design(&ipc).unwrap();
        let design = Design::extract(&imported, ArtworkScope::Board, &rules).unwrap();
        let evaluation = evaluate(0.8, SlotPlating::Plated, &design);
        assert_eq!(evaluation.measured.len(), 1);
        assert_eq!(
            evaluate(0.8, SlotPlating::Nonplated, &design).checked,
            0,
            "slot plating is part of rule selection"
        );
        let site = &evaluation.measured[0].sites[0];
        assert!(matches!(
            site.measurement_kind,
            MeasurementKind::NominalWidth
        ));
        assert_eq!(site.distance.mm, 0.6);
        assert_eq!(site.distance.uncertainty_mm, 0.0);
        let slot = site
            .evidence
            .iter()
            .find(|evidence| evidence.role == "routed_slot")
            .unwrap();
        let Some(EvidenceDisplay::Path { paths, fill_rule }) = &slot.display else {
            panic!("nominal slot evidence must retain native path data");
        };
        assert_eq!(*fill_rule, "evenodd");
        assert_eq!(paths.len(), 1);
        assert!(
            paths[0].contains('A') || paths[0].contains('C'),
            "rounded ends must stay curved"
        );
        let measured = Evidence::region("routed_slot", &design.slots[0].outline);
        assert_eq!(
            serde_json::to_value(&slot.paths).unwrap(),
            serde_json::to_value(&measured.paths).unwrap()
        );
        assert!(
            slot.paths[0].len() > 8,
            "the checked polygon remains available alongside native display geometry"
        );
    }
}
