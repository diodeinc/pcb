//! Change a document's population: the named designators are populated or
//! not, and every other designator keeps the population it was authored with.
//! Population lives only in `RefDes@populate`, so the assembly report, CPL,
//! and board arrays of the edited document all follow it.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use ipc2581::edit::{Doc, Edit, Node};

/// `xml` with `dnp` unpopulated and `populate` populated, or `None` when it
/// already is. Every other designator is left alone.
pub fn set_population(xml: &str, dnp: &[String], populate: &[String]) -> Result<Option<String>> {
    let doc = Doc::parse(xml)?;
    let references = bom_references(&doc);

    let mut wanted: HashMap<&str, bool> = HashMap::new();
    for (names, value) in [(dnp, false), (populate, true)] {
        for name in names {
            if wanted.insert(name.as_str(), value) == Some(!value) {
                bail!("Designator {name} is listed as both DNP and populated");
            }
        }
    }
    let known: HashSet<&str> = references.iter().map(|&(_, name)| name).collect();
    let mut unknown: Vec<&str> = wanted
        .keys()
        .copied()
        .filter(|n| !known.contains(n))
        .collect();
    if !unknown.is_empty() {
        unknown.sort_unstable_by(|a, b| natord::compare(a, b));
        bail!("Designators not in the BOM: {}", unknown.join(", "));
    }

    let edits = references
        .iter()
        .filter_map(|&(node, name)| {
            wanted
                .get(name)
                .map(|&value| populate_edit(&doc, node, value))
        })
        .filter_map(Result::transpose)
        .collect::<Result<Vec<_>>>()?;
    if edits.is_empty() {
        return Ok(None);
    }

    let comment = format!(
        "Population changed ({} DNP, {} populated)",
        dnp.len(),
        populate.len()
    );
    let history = crate::utils::history::file_revision_edits(&doc, &comment)?;
    let xml = doc.apply(history.into_iter().chain(edits).collect())?;
    Ok(Some(crate::utils::format::reformat_xml(&xml)?))
}

/// Every named designator of every BOM item. Population is read from all
/// BOMs, so all of them are rewritten.
fn bom_references<'a>(doc: &'a Doc<'_>) -> Vec<(Node, &'a str)> {
    doc.find_all("Bom")
        .into_iter()
        .flat_map(|bom| doc.children(bom))
        .filter(|&item| doc.name(item) == "BomItem")
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
    Ok(Some(doc.set_attr(reference, "populate", value)))
}

#[cfg(feature = "cli")]
pub fn execute(
    file: &std::path::Path,
    dnp: &[String],
    populate: &[String],
    output: &std::path::Path,
) -> Result<()> {
    use crate::utils::file as file_utils;
    use anyhow::Context as _;

    let bytes = std::fs::read(file).with_context(|| format!("Failed to read file: {file:?}"))?;
    let content = file_utils::ipc_text(file, &bytes)?;
    match set_population(&content, dnp, populate)? {
        Some(updated) => {
            file_utils::save_ipc_file(output, &updated)?;
            eprintln!(
                "Population changed ({} DNP, {} populated) in {:?}",
                dnp.len(),
                populate.len(),
                output
            );
        }
        None => {
            // Keep the input's exact bytes whenever the output is encoded the same way.
            let compressed = matches!(content, std::borrow::Cow::Owned(_));
            if compressed == (output.extension().is_some_and(|ext| ext == "zst")) {
                std::fs::write(output, &bytes)
                    .with_context(|| format!("Failed to write file: {output:?}"))?;
            } else {
                file_utils::save_ipc_file(output, &content)?;
            }
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
    fn changes_named_designators_and_leaves_the_rest() {
        let edited = set_population(XML, &dnp(&["R1"]), &dnp(&["R2"]))
            .unwrap()
            .unwrap();

        assert_eq!(
            population(&edited),
            [
                ("R1".to_string(), Some(false)),
                ("R2".to_string(), Some(true)),
                ("R3".to_string(), None),
                ("U1".to_string(), Some(true)),
                ("LOGO".to_string(), Some(false)),
            ]
        );
        assert!(edited.contains(r#"<RefDes name="R3" packageRef="R0402" layerRef="TOP"/>"#));
        assert!(edited.contains(r#"change="Population changed (1 DNP, 1 populated)""#));
    }

    #[test]
    fn unchanged_population_is_left_alone() {
        assert_eq!(
            set_population(XML, &dnp(&["R2", "LOGO"]), &dnp(&["R1", "U1"])).unwrap(),
            None
        );
        assert_eq!(set_population(XML, &[], &[]).unwrap(), None);
    }

    #[test]
    fn prefixed_designators_keep_their_prefix() {
        let xml = XML
            .replace(
                r#"<RefDes name="U1""#,
                r#"<ipc:RefDes xmlns:ipc="http://webstds.ipc.org/2581" name="U1""#,
            )
            .replace("</RefDes>", "</ipc:RefDes>");
        let edited = set_population(&xml, &dnp(&["U1"]), &[]).unwrap().unwrap();

        assert!(edited.contains(
            r#"<ipc:RefDes xmlns:ipc="http://webstds.ipc.org/2581" name="U1" packageRef="QFN" populate="false" layerRef="TOP">"#
        ));
        assert!(edited.contains("</ipc:RefDes>"));
    }

    #[test]
    fn rejects_unknown_and_conflicting_designators() {
        let error = set_population(XML, &dnp(&["R10", "R1", "C2"]), &[]).unwrap_err();
        assert_eq!(error.to_string(), "Designators not in the BOM: C2, R10");

        let error = set_population(XML, &dnp(&["R1"]), &dnp(&["R1"])).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Designator R1 is listed as both DNP and populated"
        );
    }
}
