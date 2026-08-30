//! List ICT fixture contacts from an IPC-2581 file.
//!
//! A fixture contact is a `TestPoint_ICT`-footprint component whose BOM
//! item carries an `Ict` characteristic — the `TestPoint` `ict=` config,
//! which layout sync title-cases onto the footprint and IPC exports emit
//! as a BOM characteristic. One CSV row per connected pin, at the pad
//! position when the export carries per-pad nets and the component
//! origin otherwise.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::dialects::placement::PlacementSide;
use pcb_ir::import::ipc2581::{ImportedDesign, import_design};
use pcb_ir::import::physical::Association;

use crate::accessors::IpcAccessor;
use crate::commands::cpl::CplSideFilter;
use crate::placement::extract_single_board_placements_from_design;

#[derive(Debug, Clone)]
pub struct IctOptions {
    pub output: Option<PathBuf>,
    pub side: CplSideFilter,
}

/// One fixture contact: a connected pin of an `Ict`-marked component.
#[derive(Debug, Clone)]
pub struct IctContact {
    pub designator: String,
    pub pin: String,
    pub ict: String,
    pub net: String,
    pub x: f64,
    pub y: f64,
    pub side: PlacementSide,
    pub path: String,
}

pub fn execute(file: &Path, options: &IctOptions) -> Result<()> {
    let ipc = Ipc2581::parse_file(file)?;
    let contacts = extract_contacts(&ipc)?;
    let csv = emit_ict_csv(&contacts, options.side);

    if let Some(output) = &options.output {
        fs::write(output, csv)?;
    } else {
        io::stdout().write_all(csv.as_bytes())?;
    }

    Ok(())
}

pub fn extract_contacts(ipc: &Ipc2581) -> Result<Vec<IctContact>> {
    let imported = import_design(ipc)?;
    extract_contacts_from_design(ipc, &imported)
}

fn extract_contacts_from_design(
    ipc: &Ipc2581,
    imported: &ImportedDesign,
) -> Result<Vec<IctContact>> {
    let accessor = IpcAccessor::new(ipc);

    // Designator → (ict role, zen path) from the BOM.
    let mut roles: BTreeMap<String, (String, String)> = BTreeMap::new();
    if let Some(bom) = ipc.bom() {
        for item in &bom.items {
            let Some(chars) = item.characteristics.as_ref() else {
                continue;
            };
            let data = accessor.extract_characteristics(chars);
            let Some(ict) = data
                .properties
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("ict"))
                .map(|(_, value)| value.clone())
            else {
                continue;
            };
            for ref_des in &item.ref_des_list {
                if !is_ict_package(ipc.resolve(ref_des.package_ref)) {
                    continue;
                }
                let designator = ipc.resolve(ref_des.name).to_string();
                if !designator.is_empty() {
                    roles.insert(
                        designator,
                        (ict.clone(), data.path.clone().unwrap_or_default()),
                    );
                }
            }
        }
    }

    // (designator, pin) → net and physical land location. Logical nets seed
    // connectivity when artwork omits per-pad nets; canonical physical lands
    // then provide exact board-local positions and ownership.
    #[derive(Default, Clone)]
    struct PinInfo {
        net: String,
        at: Option<(f64, f64)>,
    }
    let mut pins: BTreeMap<(String, String), PinInfo> = BTreeMap::new();
    for step in &imported.steps {
        for net in &step.logical_nets {
            for pin_ref in &net.pin_refs {
                let Some(component_ref) = pin_ref.component_ref else {
                    continue;
                };
                let key = (
                    imported.resolve(component_ref).to_string(),
                    imported.resolve(pin_ref.pin).to_string(),
                );
                pins.entry(key).or_default().net = imported.resolve(net.name).to_string();
            }
        }
        // Some assembly exports identify a test-point pad and location but
        // omit the padstack needed to form physical artwork. Retain that
        // source location as a fallback; a resolved physical land below
        // remains authoritative when one exists.
        for layer_feature in &step.layer_features {
            for set in &layer_feature.sets {
                for feature in &set.features {
                    let ipc2581::types::SetFeature::Pad(pad) = feature else {
                        continue;
                    };
                    let Some(pin_ref) = &pad.pin_ref else {
                        continue;
                    };
                    let (Some(component_ref), Some(x), Some(y)) =
                        (pin_ref.component_ref, pad.x, pad.y)
                    else {
                        continue;
                    };
                    let key = (
                        imported.resolve(component_ref).to_string(),
                        imported.resolve(pin_ref.pin).to_string(),
                    );
                    let info = pins.entry(key).or_default();
                    if let Some(net) = set.net {
                        info.net = imported.resolve(net).to_string();
                    }
                    info.at = Some((x, y));
                }
            }
        }
    }
    let lands = imported.physical_lands(ArtworkScope::Board)?;
    for land in &lands {
        let component = match &land.component {
            Association::Resolved(component) => *component,
            Association::Unresolved => continue,
            Association::Ambiguous(candidates) => bail!(
                "ICT land has ambiguous component ownership ({} candidates)",
                candidates.len()
            ),
            Association::Conflicting(candidates) => bail!(
                "ICT land has conflicting component ownership ({} candidates)",
                candidates.len()
            ),
        };
        let Some(pin) = land.pin else {
            continue;
        };
        let component = imported
            .component_definition(component.component)
            .expect("physical land component references a canonical definition");
        let Some(reference) = component.source.ref_des else {
            continue;
        };
        let key = (
            imported.resolve(reference).to_string(),
            imported.resolve(pin).to_string(),
        );
        let info = pins.entry(key).or_default();
        if let Some(net) = land.net {
            info.net = imported.resolve(net).to_string();
        }
        info.at = Some((land.at.x, land.at.y));
    }

    let placements = extract_single_board_placements_from_design(&accessor, imported)?;
    let mut contacts = Vec::new();
    for component in &placements.components {
        let Some((ict, path)) = roles.get(&component.designator) else {
            continue;
        };
        let empty = PinInfo::default();
        let mut rows: Vec<(&String, &PinInfo)> = pins
            .range((component.designator.clone(), String::new())..)
            .take_while(|((designator, _), _)| designator == &component.designator)
            .map(|((_, pin), info)| (pin, info))
            .collect();
        // A contact with no connected pin is still a contact — keep it
        // visible instead of silently dropping it.
        if rows.is_empty() {
            rows.push((&EMPTY, &empty));
        }
        for (pin, info) in rows {
            let (x, y) = info.at.unwrap_or((component.at.x, component.at.y));
            contacts.push(IctContact {
                designator: component.designator.clone(),
                pin: pin.clone(),
                ict: ict.clone(),
                net: info.net.clone(),
                x,
                y,
                side: component.side,
                path: path.clone(),
            });
        }
    }

    contacts.sort_by(compare_contacts);
    Ok(contacts)
}

static EMPTY: String = String::new();

/// The `TestPoint` ICT variant's footprint, allowing the `_<n>` suffix
/// board-array creation appends when deduplicating package names.
/// Mirrored in `pcb_interposer::contacts`.
pub fn is_ict_package(name: &str) -> bool {
    const FOOTPRINT: &str = "TestPoint_ICT";
    match name.strip_prefix(FOOTPRINT) {
        Some("") => true,
        Some(rest) => rest
            .strip_prefix('_')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())),
        None => false,
    }
}

fn compare_contacts(left: &IctContact, right: &IctContact) -> Ordering {
    side_sort_key(left.side)
        .cmp(&side_sort_key(right.side))
        .then_with(|| natord::compare(&left.designator, &right.designator))
        .then_with(|| natord::compare(&left.pin, &right.pin))
}

fn side_sort_key(side: PlacementSide) -> u8 {
    match side {
        PlacementSide::Top => 0,
        PlacementSide::Bottom => 1,
        PlacementSide::Internal => 2,
        PlacementSide::Unknown => 3,
    }
}

pub fn emit_ict_csv(contacts: &[IctContact], side: CplSideFilter) -> String {
    let mut output = String::from("Designator,Pin,Ict,Net,X,Y,Side,Path\n");
    for contact in contacts {
        let keep = match side {
            CplSideFilter::Both => true,
            CplSideFilter::Top => contact.side == PlacementSide::Top,
            CplSideFilter::Bottom => contact.side == PlacementSide::Bottom,
        };
        if !keep {
            continue;
        }
        write_csv_row(
            &mut output,
            &[
                contact.designator.as_str(),
                contact.pin.as_str(),
                contact.ict.as_str(),
                contact.net.as_str(),
                &format!("{:.6}", contact.x),
                &format!("{:.6}", contact.y),
                side_name(contact.side),
                contact.path.as_str(),
            ],
        );
    }
    output
}

fn side_name(side: PlacementSide) -> &'static str {
    match side {
        PlacementSide::Top => "top",
        PlacementSide::Bottom => "bottom",
        PlacementSide::Internal => "internal",
        PlacementSide::Unknown => "unknown",
    }
}

fn write_csv_row(output: &mut String, fields: &[&str]) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write_csv_field(output, field);
    }
    output.push('\n');
}

fn write_csv_field(output: &mut String, field: &str) {
    if !field.contains([',', '"', '\n', '\r']) {
        output.push_str(field);
        return;
    }

    output.push('"');
    for ch in field.chars() {
        if ch == '"' {
            output.push('"');
        }
        output.push(ch);
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ict_package_names_match_with_dedupe_suffix() {
        assert!(is_ict_package("TestPoint_ICT"));
        assert!(is_ict_package("TestPoint_ICT_35"));
        assert!(!is_ict_package("TestPoint_ICT_"));
        assert!(!is_ict_package("TestPoint_ICT_x"));
        assert!(!is_ict_package("TestPoint_Pad_D1.0mm"));
        assert!(!is_ict_package("Pad_D1.0mm"));
    }

    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <DictionaryStandard units="MILLIMETER"/>
  </Content>
  <Bom name="BOM_board">
    <BomHeader assembly="board" revision="1.0">
      <StepRef name="board"/>
    </BomHeader>
    <BomItem OEMDesignNumberRef="TP_GND" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP1" packageRef="TestPoint_ICT" populate="true" layerRef="BOTTOM"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Ict" textualCharacteristicValue="gnd"/>
        <Textual textualCharacteristicName="Path" textualCharacteristicValue="TP_GND"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="R10k" quantity="1" pinCount="2" category="ELECTRICAL">
      <RefDes name="R1" packageRef="R_0603" populate="true" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Value" textualCharacteristicValue="10k"/>
      </Characteristics>
    </BomItem>
  </Bom>
  <Ecad name="ecad">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Stackup name="stackup" overallThickness="1.6" tolPlus="0.0" tolMinus="0.0">
        <StackupGroup name="group" thickness="1.6" tolPlus="0.0" tolMinus="0.0"/>
      </Stackup>
      <Step name="board">
        <Datum x="0.0" y="0.0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0.0" y="0.0"/>
            <PolyStepSegment x="10.0" y="0.0"/>
            <PolyStepSegment x="10.0" y="10.0"/>
            <PolyStepSegment x="0.0" y="10.0"/>
            <PolyStepSegment x="0.0" y="0.0"/>
          </Polygon>
        </Profile>
        <LogicalNet name="GND">
          <PinRef pin="1" componentRef="TP1"/>
          <PinRef pin="2" componentRef="R1"/>
        </LogicalNet>
        <LogicalNet name="SIG">
          <PinRef pin="1" componentRef="R1"/>
        </LogicalNet>
        <Component refDes="TP1" packageRef="TestPoint_ICT" layerRef="BOTTOM" part="TP_GND" mountType="SMT">
          <Location x="3.500" y="4.250"/>
        </Component>
        <Component refDes="R1" packageRef="R_0603" layerRef="TOP" part="R10k" mountType="SMT">
          <Location x="1.000" y="2.000"/>
        </Component>
        <LayerFeature layerRef="BOTTOM">
          <Set net="GND">
            <Pad>
              <Location x="3.600" y="4.300"/>
              <PinRef componentRef="TP1" pin="1"/>
            </Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

    #[test]
    fn lists_ict_contacts_with_nets() {
        let ipc = Ipc2581::parse(FIXTURE).expect("fixture parses");
        let contacts = extract_contacts(&ipc).expect("extracts");

        assert_eq!(contacts.len(), 1);
        let contact = &contacts[0];
        assert_eq!(contact.designator, "TP1");
        assert_eq!(contact.pin, "1");
        assert_eq!(contact.ict, "gnd");
        assert_eq!(contact.net, "GND");
        assert_eq!(contact.side, PlacementSide::Bottom);
        // The pad Set position wins over the component origin.
        assert_eq!((contact.x, contact.y), (3.6, 4.3));
        assert_eq!(contact.path, "TP_GND");

        let csv = emit_ict_csv(&contacts, CplSideFilter::Both);
        assert_eq!(
            csv,
            "Designator,Pin,Ict,Net,X,Y,Side,Path\n\
             TP1,1,gnd,GND,3.600000,4.300000,bottom,TP_GND\n"
        );
        assert_eq!(
            emit_ict_csv(&contacts, CplSideFilter::Top),
            "Designator,Pin,Ict,Net,X,Y,Side,Path\n"
        );
    }
}
