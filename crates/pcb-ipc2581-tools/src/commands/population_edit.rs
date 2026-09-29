//! Override a document's population: every BOM designator is populated except
//! a given do-not-populate set. Population lives only in `RefDes@populate`, so
//! the assembly report, CPL, and board arrays of the edited document all
//! follow it.

use std::collections::HashSet;

use anyhow::{Result, bail};
use ipc2581::XmlWriter;
use ipc2581::edit::{Doc, Edit, Node};

/// `xml` with every non-document BOM designator populated except those in
/// `dnp`, or `None` when it already has exactly that population.
pub fn set_population(xml: &str, dnp: &[String]) -> Result<Option<String>> {
    let doc = Doc::parse(xml)?;
    let references = bom_references(&doc);
    let dnp: HashSet<&str> = dnp.iter().map(String::as_str).collect();

    let known: HashSet<&str> = references.iter().map(|&(_, name)| name).collect();
    let mut unknown: Vec<&str> = dnp.difference(&known).copied().collect();
    if !unknown.is_empty() {
        unknown.sort_unstable_by(|a, b| natord::compare(a, b));
        bail!("DNP designators not in the BOM: {}", unknown.join(", "));
    }

    let edits = references
        .iter()
        .map(|&(reference, name)| populate_edit(&doc, reference, !dnp.contains(name)))
        .filter_map(Result::transpose)
        .collect::<Result<Vec<_>>>()?;
    if edits.is_empty() {
        return Ok(None);
    }

    let comment = format!(
        "Population set ({} of {} designators DNP)",
        dnp.len(),
        known.len()
    );
    let history = crate::utils::history::file_revision_edits(&doc, &comment)?;
    let xml = doc.apply(history.into_iter().chain(edits).collect())?;
    Ok(Some(crate::utils::format::reformat_xml(&xml)?))
}

/// Every named designator of every non-document BOM item. Population is read
/// from all BOMs, so all of them are rewritten.
fn bom_references<'a>(doc: &'a Doc<'_>) -> Vec<(Node, &'a str)> {
    doc.find_all("Bom")
        .into_iter()
        .flat_map(|bom| doc.children(bom))
        .filter(|&item| {
            doc.name(item) == "BomItem"
                && doc.attr(item, "category").map(str::trim) != Some("DOCUMENT")
        })
        .flat_map(|item| doc.children(item))
        .filter(|&child| doc.name(child) == "RefDes")
        .filter_map(|reference| {
            let name = doc.attr(reference, "name")?;
            (!name.is_empty()).then_some((reference, name))
        })
        .collect()
}

/// The start-tag rewrite giving `reference` the population `populate`, or
/// `None` when it already has it.
fn populate_edit(doc: &Doc<'_>, reference: Node, populate: bool) -> Result<Option<Edit>> {
    let current = doc
        .attr(reference, "populate")
        .map(|value| match value.trim() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => bail!("Invalid RefDes populate value {value:?}"),
        })
        .transpose()?;
    if current == Some(populate) {
        return Ok(None);
    }

    let value = if populate { "true" } else { "false" };
    let mut attrs = doc
        .attrs(reference)
        .map(|(name, old)| (name.to_string(), old.to_string()))
        .collect::<Vec<_>>();
    match attrs.iter_mut().find(|(name, _)| name == "populate") {
        Some((_, old)) => *old = value.to_string(),
        None => attrs.push(("populate".to_string(), value.to_string())),
    }

    let mut writer = XmlWriter::new();
    if doc.source(reference).ends_with("/>") {
        writer.empty_element_with("RefDes", attrs);
    } else {
        writer.start_element_with("RefDes", attrs);
    }
    Ok(Some(doc.replace_start_tag(reference, writer.into_string())))
}

#[cfg(feature = "cli")]
pub fn execute(file: &std::path::Path, dnp: &[String], output: &std::path::Path) -> Result<()> {
    use crate::utils::file as file_utils;

    let content = file_utils::load_ipc_file(file)?;
    match set_population(&content, dnp)? {
        Some(updated) => {
            file_utils::save_ipc_file(output, &updated)?;
            eprintln!("Set population ({} DNP) in {:?}", dnp.len(), output);
        }
        None => {
            file_utils::save_ipc_file(output, &content)?;
            eprintln!("Population unchanged in {:?}", output);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="board"/>
    <BomRef name="bom"/>
  </Content>
  <LogisticHeader>
    <Role id="Owner" roleFunction="SENDER"/>
    <Enterprise id="Diode" code="NONE"/>
    <Person name="pcb" enterpriseRef="Diode" roleRef="Owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="KiCad" lastChange="2026-01-01T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="" label="">
      <SoftwarePackage name="KiCad" revision="10.0.0" vendor="KiCad EDA"/>
    </FileRevision>
  </HistoryRecord>
  <Bom name="bom">
    <BomHeader assembly="board" revision="1"><StepRef name="board"/></BomHeader>
    <BomItem OEMDesignNumberRef="r" quantity="3" category="ELECTRICAL">
      <RefDes name="R1" packageRef="R0402" populate="true" layerRef="TOP"/>
      <RefDes name="R2" packageRef="R0402" populate="false" layerRef="TOP"/>
      <RefDes name="R3" packageRef="R0402" layerRef="TOP"/>
    </BomItem>
    <BomItem OEMDesignNumberRef="u" quantity="1" category="ELECTRICAL">
      <RefDes name="U1" packageRef="QFN" populate="1" layerRef="TOP">
        <Tuning value="trim"/>
      </RefDes>
    </BomItem>
    <BomItem OEMDesignNumberRef="logo" quantity="1" category="DOCUMENT">
      <RefDes name="LOGO" packageRef="art" populate="false" layerRef="TOP"/>
    </BomItem>
  </Bom>
</IPC-2581>
"#;

    fn dnp(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn population(xml: &str) -> Vec<(String, Option<bool>)> {
        let ipc = ipc2581::Ipc2581::parse(xml).unwrap();
        ipc.bom()
            .unwrap()
            .items
            .iter()
            .flat_map(|item| item.reference_designators())
            .map(|reference| (ipc.resolve(reference.name).to_string(), reference.populate))
            .collect()
    }

    #[test]
    fn sets_listed_dnp_and_populates_the_rest() {
        let edited = set_population(XML, &dnp(&["R1", "U1"])).unwrap().unwrap();

        assert_eq!(
            population(&edited),
            [
                ("R1".to_string(), Some(false)),
                ("R2".to_string(), Some(true)),
                ("R3".to_string(), Some(true)),
                ("U1".to_string(), Some(false)),
                ("LOGO".to_string(), Some(false)),
            ]
        );
        assert!(
            edited.contains(
                r#"<RefDes name="R3" packageRef="R0402" layerRef="TOP" populate="true"/>"#
            )
        );
        assert!(edited.contains(
            "<RefDes name=\"U1\" packageRef=\"QFN\" populate=\"false\" layerRef=\"TOP\">\n        <Tuning value=\"trim\"/>"
        ));
        assert!(edited.contains(r#"change="Population set (2 of 4 designators DNP)""#));
    }

    #[test]
    fn unchanged_population_is_left_alone() {
        let edited = set_population(XML, &dnp(&["R1"])).unwrap().unwrap();

        assert_eq!(set_population(&edited, &dnp(&["R1", "R1"])).unwrap(), None);
    }

    #[test]
    fn rejects_designators_outside_the_bom() {
        let error = set_population(XML, &dnp(&["R10", "LOGO", "R1", "C2"])).unwrap_err();

        assert_eq!(
            error.to_string(),
            "DNP designators not in the BOM: C2, LOGO, R10"
        );
    }
}
