#[cfg(feature = "cli")]
use std::collections::HashMap;
#[cfg(feature = "cli")]
use std::path::Path;

#[cfg(feature = "cli")]
use anyhow::{Context, Result};
#[cfg(feature = "cli")]
use pcb_sch::bom::{
    Alternative as SchAlternative, Bom, BomEntry, Capacitor, GenericComponent, Resistor,
    trim_description,
};

#[cfg(feature = "cli")]
use crate::OutputFormat;
use crate::accessors::{CharacteristicsData, IpcAccessor};
#[cfg(feature = "cli")]
use crate::utils::file as file_utils;
use serde::Serialize;

/// Build GenericComponent from extracted characteristics
/// Reuses the same logic as detect_generic_component in pcb-sch
#[cfg(feature = "cli")]
fn build_generic_component(data: &CharacteristicsData) -> Option<GenericComponent> {
    match data.component_type.as_deref()? {
        "resistor" => {
            let resistance = data.resistance.as_ref()?.parse().ok()?;
            let voltage = data.voltage.as_ref().and_then(|v| v.parse().ok());
            Some(GenericComponent::Resistor(Resistor {
                resistance,
                voltage,
                power: None,
            }))
        }
        "capacitor" => {
            let capacitance = data.capacitance.as_ref()?.parse().ok()?;
            let dielectric = data.dielectric.as_ref().and_then(|d| d.parse().ok());
            let esr = data.esr.as_ref().and_then(|e| e.parse().ok());
            let voltage = data.voltage.as_ref().and_then(|v| v.parse().ok());
            Some(GenericComponent::Capacitor(Capacitor {
                capacitance,
                dielectric,
                esr,
                voltage,
            }))
        }
        _ => None,
    }
}

#[cfg(feature = "cli")]
pub fn execute(file: &Path, format: OutputFormat, offline: bool) -> Result<()> {
    let content = file_utils::load_ipc_file(file)?;
    let ipc = ipc2581::Ipc2581::parse(&content)?;
    let accessor = IpcAccessor::new(&ipc);

    let mut bom = extract_bom_from_ipc(&accessor);

    if !offline {
        use pcb_ui::prelude::*;
        let file_name = file.file_name().unwrap_or_default().to_string_lossy();
        let spinner = Spinner::builder(format!("{file_name}: Fetching availability")).start();
        let ctx = pcb_diode_api::WorkspaceContext::from_path(file);

        let token = pcb_diode_api::auth::get_api_token_with_context(&ctx)
            .context("Not authenticated. Run `pcb auth login` to authenticate.")?;

        if let Err(e) = pcb_diode_api::fetch_and_populate_availability_with_context(
            &ctx,
            token.as_deref(),
            &mut bom,
            false,
        ) {
            log::warn!("Failed to fetch availability data: {}", e);
        }

        spinner.finish();
    }

    pcb_ui::write_stdout(|writer| match format {
        OutputFormat::Json => {
            write!(writer, "{}", bom.ungrouped_json())
        }
        OutputFormat::Text => bom.write_table(writer),
    })?;

    Ok(())
}

/// One populated or DNP component instance, resolved through the IPC BOM and AVL.
#[derive(Debug, Clone, Serialize)]
pub struct BomLine {
    pub designator: String,
    pub path: String,
    pub mpn: Option<String>,
    pub manufacturer: Option<String>,
    pub description: Option<String>,
    pub dnp: bool,
    /// All extracted characteristics, including the original engineering-unit
    /// strings used by the CLI's typed generic-component adapter.
    pub characteristics: CharacteristicsData,
}

/// Resolve BOM instances without network requests or filesystem access.
/// DOCUMENT entries are excluded; DNP entries remain explicitly marked.
pub fn extract_bom_lines(accessor: &IpcAccessor) -> Vec<BomLine> {
    let ipc = accessor.ipc();
    let mut lines = Vec::new();
    if let Some(bom) = ipc.bom() {
        for item in &bom.items {
            if matches!(item.category, Some(ipc2581::types::BomCategory::Document)) {
                continue;
            }
            let mut characteristics = item
                .characteristics
                .as_ref()
                .map(|chars| accessor.extract_characteristics(chars))
                .unwrap_or_default();
            let mut avl = accessor.lookup_avl(item.oem_design_number_ref);
            avl.alternatives.append(&mut characteristics.alternatives);
            characteristics.alternatives = avl.alternatives;
            let description = item
                .description
                .map(|symbol| ipc.resolve(symbol).to_string())
                .or_else(|| characteristics.value.clone());
            for ref_des in &item.ref_des_list {
                let designator = ipc.resolve(ref_des.name);
                if designator.is_empty() {
                    continue;
                }
                lines.push(BomLine {
                    designator: designator.to_string(),
                    path: characteristics
                        .path
                        .clone()
                        .unwrap_or_else(|| format!("ipc::{designator}")),
                    mpn: avl.primary_mpn.clone(),
                    manufacturer: avl.primary_manufacturer.clone(),
                    description: description.clone(),
                    dnp: !ref_des.populate,
                    characteristics: characteristics.clone(),
                });
            }
        }
    }
    if lines.is_empty()
        && let Some(step) = accessor.first_step()
    {
        for component in &step.components {
            let Some(ref_des) = component.ref_des else {
                continue;
            };
            let designator = ipc.resolve(ref_des);
            if designator.is_empty() {
                continue;
            }
            lines.push(BomLine {
                designator: designator.to_string(),
                path: format!("ipc::{designator}"),
                mpn: Some(ipc.resolve(component.part).to_string()).filter(|part| !part.is_empty()),
                manufacturer: None,
                description: None,
                dnp: false,
                characteristics: CharacteristicsData {
                    package: component
                        .package_ref
                        .map(|package| ipc.resolve(package).to_string())
                        .filter(|package| !package.is_empty()),
                    ..Default::default()
                },
            });
        }
    }
    lines
}

#[cfg(feature = "cli")]
fn extract_bom_from_ipc(accessor: &IpcAccessor) -> Bom {
    let mut entries = HashMap::new();
    let mut designators = HashMap::new();
    for line in extract_bom_lines(accessor) {
        let generic_data = build_generic_component(&line.characteristics);
        entries.insert(
            line.path.clone(),
            BomEntry {
                mpn: line.mpn,
                alternatives: line
                    .characteristics
                    .alternatives
                    .into_iter()
                    .map(|alt| SchAlternative {
                        mpn: alt.mpn,
                        manufacturer: alt.manufacturer,
                    })
                    .collect(),
                manufacturer: line.manufacturer,
                package: line.characteristics.package,
                value: line.characteristics.value,
                description: trim_description(line.description),
                generic_data,
                dnp: line.dnp,
                skip_bom: false,
                properties: line.characteristics.properties,
            },
        );
        designators.insert(line.path, line.designator);
    }
    Bom::new(entries, designators)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_bom_resolves_avl_and_keeps_dnp_instances() {
        let ipc = ipc2581::Ipc2581::parse(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="ASSEMBLY"/></Content>
  <LogisticHeader>
    <Enterprise id="mfr" name="Example Components" code="MFR"/>
  </LogisticHeader>
  <Bom name="bom">
    <BomHeader assembly="board" revision="1"/>
    <BomItem OEMDesignNumberRef="R" quantity="2" category="ELECTRICAL" description="  resistor description  ">
      <RefDes name="R10" packageRef="R0402" populate="false" layerRef="F.Cu"/>
      <RefDes name="R2" packageRef="R0402" populate="true" layerRef="F.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="pcb" textualCharacteristicName="Package" textualCharacteristicValue="0402"/>
        <Textual definitionSource="pcb" textualCharacteristicName="Value" textualCharacteristicValue="1kohm"/>
        <Textual definitionSource="pcb" textualCharacteristicName="Type" textualCharacteristicValue="resistor"/>
        <Textual definitionSource="pcb" textualCharacteristicName="Resistance" textualCharacteristicValue="1kohm"/>
        <Textual definitionSource="pcb" textualCharacteristicName="Voltage" textualCharacteristicValue="50V"/>
        <Textual definitionSource="pcb" textualCharacteristicName="Tolerance" textualCharacteristicValue="1%"/>
        <Textual definitionSource="pcb" textualCharacteristicName="Alternatives" textualCharacteristicValue="{&quot;mpn&quot;:&quot;TEXTUAL&quot;,&quot;manufacturer&quot;:&quot;Example Components&quot;}"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TP" quantity="1" category="DOCUMENT">
      <RefDes name="TP1" packageRef="TestPoint" populate="true" layerRef="F.Cu"/>
    </BomItem>
  </Bom>
  <Avl name="avl">
    <AvlItem OEMDesignNumber="R">
      <AvlVmpn qualified="true" chosen="false"><AvlMpn name="ALTERNATIVE"/><AvlVendor enterpriseRef="mfr"/></AvlVmpn>
      <AvlVmpn qualified="true" chosen="true"><AvlMpn name="PRIMARY"/><AvlVendor enterpriseRef="mfr"/></AvlVmpn>
    </AvlItem>
  </Avl>
</IPC-2581>"#,
        )
        .unwrap();
        let accessor = IpcAccessor::new(&ipc);
        let lines = extract_bom_lines(&accessor);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].designator, "R10");
        assert!(lines[0].dnp);
        assert!(!lines[1].dnp);
        assert_eq!(lines[1].path, "ipc::R2");
        assert_eq!(lines[1].mpn.as_deref(), Some("PRIMARY"));
        assert_eq!(lines[1].manufacturer.as_deref(), Some("Example Components"));
        assert_eq!(lines[1].characteristics.alternatives[0].mpn, "ALTERNATIVE");
        assert_eq!(lines[1].characteristics.alternatives[1].mpn, "TEXTUAL");
        assert_eq!(lines[1].characteristics.package.as_deref(), Some("0402"));
        assert_eq!(lines[1].characteristics.properties["Tolerance"], "1%");
        let serialized = serde_json::to_value(&lines).unwrap();
        assert_eq!(serialized[0]["dnp"], true);
        assert_eq!(serialized[0]["characteristics"]["resistance"], "1kohm");
        assert_eq!(serialized[0]["characteristics"]["voltage"], "50V");
        assert_eq!(
            serialized[0]["characteristics"]["component_type"],
            "resistor"
        );
        assert_eq!(
            serde_json::to_string(&lines).unwrap(),
            serde_json::to_string(&extract_bom_lines(&accessor)).unwrap(),
        );

        #[cfg(feature = "cli")]
        {
            let bom = extract_bom_from_ipc(&accessor);
            assert!(bom.entries["ipc::R10"].dnp);
            assert_eq!(bom.entries["ipc::R2"].alternatives[1].mpn, "TEXTUAL");
            assert_eq!(
                bom.entries["ipc::R2"].description.as_deref(),
                Some("resistor description")
            );
            assert!(matches!(
                bom.entries["ipc::R2"].generic_data,
                Some(GenericComponent::Resistor(_))
            ));
        }
    }
}
