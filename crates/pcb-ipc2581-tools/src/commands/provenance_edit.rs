//! Record a production run's provenance in the file itself: the parties of
//! its LogisticHeader, `Content@roleRef`, its BOM header, its HistoryRecord
//! attributes and a named CadHeader Spec of `General type="CONFIGURATION"`
//! properties. Read it back with [`crate::accessors::IpcAccessor::provenance`].

use anyhow::{Context as _, Result, bail};
use ipc2581::XmlWriter;
use ipc2581::edit::{Doc, Edit, Node};
use serde::Deserialize;

use crate::accessors::{
    BomHeaderInfo, ConfigurationSpec, EnterpriseInfo, HistoryInfo, PersonDetails, PropertyValue,
    RoleInfo,
};

/// The input of `ipc edit provenance`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvenanceInput {
    /// LogisticHeader parties, upserted by Role id, Enterprise id and Person name.
    #[serde(default)]
    pub parties: Vec<Party>,
    /// `BomHeader@assembly` and `@revision` of every BOM.
    pub bom: Option<BomHeaderInfo>,
    /// HistoryRecord attributes; those left out keep their value.
    #[serde(default)]
    pub history: HistoryInfo,
    /// The CadHeader Spec to write, replacing a Spec of the same name.
    pub spec: Option<ConfigurationSpec>,
    /// The Person the history ChangeRec is attributed to.
    pub changed_by: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Party {
    pub role: RoleInfo,
    pub enterprise: EnterpriseInfo,
    #[serde(default)]
    pub persons: Vec<PersonDetails>,
}

/// The kinds of LogisticHeader entry, in schema order, with the attribute
/// that keys each and KiCad's placeholder key.
const KINDS: [(&str, &str, &str); 3] = [
    ("Role", "id", "Owner"),
    ("Enterprise", "id", "UNKNOWN"),
    ("Person", "name", "UNKNOWN"),
];

/// The attributes of each kind of [`KINDS`] the input sets.
const MODELLED: [&[&str]; 3] = [
    &["id", "roleFunction", "description", "authority"],
    &[
        "id",
        "name",
        "code",
        "codeType",
        "address1",
        "address2",
        "city",
        "stateProvince",
        "country",
        "postalCode",
        "phone",
        "fax",
        "email",
        "url",
    ],
    &[
        "name",
        "enterpriseRef",
        "title",
        "email",
        "phone",
        "fax",
        "mailstop",
        "roleRef",
    ],
];

/// `xml` with the provenance of `input` recorded.
pub fn set_provenance(xml: &str, input: &ProvenanceInput) -> Result<String> {
    let entries = header_entries(input)?;
    check_enumerations(input)?;

    let doc = Doc::parse(xml)?;
    let root = doc.root()?;
    let content = doc
        .child(root, "Content")
        .context("IPC-2581 has no Content")?;
    let header = doc.child(root, "LogisticHeader");
    let existing = |kind: &str| -> Vec<Node> {
        header
            .map(|header| doc.children(header))
            .unwrap_or_default()
            .into_iter()
            .filter(|&child| doc.name(child) == kind)
            .collect()
    };

    let mut owners: Vec<&str> = Vec::new();
    for role in input.parties.iter().map(|party| &party.role) {
        if role.role_function == "OWNER" && !owners.contains(&role.id.as_str()) {
            owners.push(&role.id);
        }
    }
    let owner = match owners[..] {
        [] => None,
        [owner] => Some(owner),
        _ => bail!("More than one OWNER role: {}", owners.join(", ")),
    };

    let removed = unreferenced_placeholders(&doc, &entries, owner.is_some());
    let mut edits = Vec::new();
    let mut header_xml = XmlWriter::new();
    for (kind_index, &(kind, key, _)) in KINDS.iter().enumerate() {
        let nodes = existing(kind);
        let mut new = XmlWriter::new();
        let mut kept = 0;
        for (name, attrs) in entries.iter().filter(|(name, _)| *name == kind) {
            let id = &attrs[0].1;
            match nodes.iter().find(|&&node| doc.attr(node, key) == Some(id)) {
                Some(&node) => {
                    // Attributes the input does not model, such as publicKey, stay.
                    let modelled = MODELLED[kind_index];
                    let kept_attrs = doc.attrs(node).filter(|(k, _)| !modelled.contains(k));
                    let mut writer = XmlWriter::new();
                    writer.empty_element_with(
                        name,
                        attrs
                            .iter()
                            .map(|(k, v)| (*k, v.as_str()))
                            .chain(kept_attrs),
                    );
                    edits.push(doc.replace(node, writer.into_string()));
                }
                None => new.empty_element_with(name, attrs.iter().map(|(k, v)| (*k, v.as_str()))),
            }
            kept += 1;
        }
        for &node in &nodes {
            if removed.contains(&node) {
                edits.push(doc.delete(node));
            } else if !entries
                .iter()
                .any(|(name, attrs)| *name == kind && doc.attr(node, key) == Some(&attrs[0].1))
            {
                kept += 1;
            }
        }
        if kept == 0 {
            bail!("LogisticHeader needs at least one {kind}");
        }

        let new = new.into_string();
        if new.is_empty() {
            continue;
        }
        let Some(header) = header else {
            header_xml.raw(&new);
            continue;
        };
        // New entries follow the last of their kind, or precede the next kind.
        let later = KINDS[kind_index + 1..]
            .iter()
            .find_map(|&(later, _, _)| existing(later).first().copied());
        edits.push(match (nodes.last(), later) {
            (Some(&last), _) => doc.insert_after(last, new),
            (None, Some(next)) => doc.insert_before(next, new),
            (None, None) => doc.append_inside(header, new),
        });
    }
    if header.is_none() {
        let mut writer = XmlWriter::new();
        writer.start_element("LogisticHeader", &[]);
        writer.raw(&header_xml.into_string());
        writer.end_element("LogisticHeader");
        edits.push(doc.insert_after(content, writer.into_string()));
    }

    let person_known = entries
        .iter()
        .any(|(kind, attrs)| *kind == "Person" && attrs[0].1 == input.changed_by)
        || existing("Person").into_iter().any(|node| {
            doc.attr(node, "name") == Some(input.changed_by.as_str()) && !removed.contains(&node)
        });
    if !person_known {
        bail!("changedBy names no Person: {}", input.changed_by);
    }

    if let Some(owner) = owner {
        edits.push(doc.set_attr(content, "roleRef", owner));
    }

    if let Some(bom) = &input.bom {
        let headers: Vec<_> = doc
            .find_all("Bom")
            .into_iter()
            .filter_map(|bom| doc.child(bom, "BomHeader"))
            .collect();
        if headers.is_empty() {
            bail!("IPC-2581 has no BomHeader to record the assembly in");
        }
        for header in headers {
            edits.push(doc.set_attr(header, "assembly", &bom.assembly));
            edits.push(doc.set_attr(header, "revision", &bom.revision));
        }
    }

    let history_attrs: Vec<(&str, &str)> = [
        ("lifecyclePhase", &input.history.lifecycle_phase),
        (
            "externalConfigurationEntryPoint",
            &input.history.external_configuration_entry_point,
        ),
    ]
    .into_iter()
    .filter_map(|(key, value)| Some((key, value.as_deref()?)))
    .collect();
    edits.extend(crate::utils::history::file_revision_edits_by(
        &doc,
        "Provenance updated",
        &input.changed_by,
        &history_attrs,
    )?);

    if let Some(spec) = &input.spec {
        edits.push(spec_edit(&doc, spec)?);
    }

    let xml = crate::utils::format::reformat_xml(&doc.apply(edits)?)?;
    ipc2581::Ipc2581::parse(&xml).context("IPC-2581 with provenance did not parse")?;
    Ok(xml)
}

/// The LogisticHeader entries of `input`, each as its element name and its
/// attributes, the key first. An entry listed twice must be listed the same.
fn header_entries(input: &ProvenanceInput) -> Result<Vec<(&'static str, Attrs)>> {
    let mut entries: Vec<(&'static str, Attrs)> = Vec::new();
    let mut add = |kind: &'static str, attrs: Attrs| match entries
        .iter()
        .find(|(name, other)| *name == kind && other[0].1 == attrs[0].1)
    {
        Some((_, other)) if *other == attrs => Ok(()),
        Some(_) => bail!("{kind} {} is given twice, differently", attrs[0].1),
        None => {
            entries.push((kind, attrs));
            Ok(())
        }
    };
    for party in &input.parties {
        let RoleInfo {
            id,
            role_function,
            description,
            authority,
        } = &party.role;
        add(
            "Role",
            attrs([
                ("id", Some(id)),
                ("roleFunction", Some(role_function)),
                ("description", description.as_ref()),
                ("authority", authority.as_ref()),
            ]),
        )?;
        let e = &party.enterprise;
        add(
            "Enterprise",
            attrs([
                ("id", Some(&e.id)),
                ("name", e.name.as_ref()),
                ("code", Some(&e.code)),
                ("codeType", e.code_type.as_ref()),
                ("address1", e.address1.as_ref()),
                ("address2", e.address2.as_ref()),
                ("city", e.city.as_ref()),
                ("stateProvince", e.state_province.as_ref()),
                ("country", e.country.as_ref()),
                ("postalCode", e.postal_code.as_ref()),
                ("phone", e.phone.as_ref()),
                ("fax", e.fax.as_ref()),
                ("email", e.email.as_ref()),
                ("url", e.url.as_ref()),
            ]),
        )?;
        for p in &party.persons {
            add(
                "Person",
                attrs([
                    ("name", Some(&p.name)),
                    ("enterpriseRef", Some(&e.id)),
                    ("title", p.title.as_ref()),
                    ("email", p.email.as_ref()),
                    ("phone", p.phone.as_ref()),
                    ("fax", p.fax.as_ref()),
                    ("mailstop", p.mailstop.as_ref()),
                    ("roleRef", Some(id)),
                ]),
            )?;
        }
    }
    Ok(entries)
}

type Attrs = Vec<(&'static str, String)>;

fn attrs<const N: usize>(attrs: [(&'static str, Option<&String>); N]) -> Attrs {
    attrs
        .into_iter()
        .filter_map(|(key, value)| Some((key, value?.clone())))
        .collect()
}

/// KiCad's placeholder entries that nothing else will reference once the
/// edit is applied. A placeholder stays when the input upserts it, when the
/// input writes no entry of its kind, or when anything outside the removed
/// placeholders refers to it: a Person, `Content@roleRef` (unless an OWNER
/// replaces it), an AVL vendor, a ChangeRec or Approval, or a BOM
/// characteristic naming an enterprise.
fn unreferenced_placeholders(
    doc: &Doc,
    entries: &[(&'static str, Attrs)],
    content_role_replaced: bool,
) -> Vec<Node> {
    let Some(header) = doc
        .root()
        .ok()
        .and_then(|root| doc.child(root, "LogisticHeader"))
    else {
        return Vec::new();
    };
    let mut removed: Vec<Node> = doc
        .children(header)
        .into_iter()
        .filter(|&node| {
            KINDS.iter().any(|&(kind, key, placeholder)| {
                doc.name(node) == kind
                    && doc.attr(node, key) == Some(placeholder)
                    && entries.iter().any(|(name, _)| *name == kind)
                    && !entries
                        .iter()
                        .any(|(name, attrs)| *name == kind && attrs[0].1 == placeholder)
            })
        })
        .collect();

    // An upserted entry refers to its party's role and enterprise instead.
    let upserted = |node: Node| {
        KINDS.iter().any(|&(kind, key, _)| {
            doc.name(node) == kind
                && entries
                    .iter()
                    .any(|(name, attrs)| *name == kind && doc.attr(node, key) == Some(&attrs[0].1))
        })
    };
    // (referring element, kind referred to, attribute holding the reference)
    let references = [
        ("Person", "Role", "roleRef"),
        ("Person", "Enterprise", "enterpriseRef"),
        ("Content", "Role", "roleRef"),
        ("AvlVendor", "Enterprise", "enterpriseRef"),
        ("ChangeRec", "Person", "personRef"),
        ("Approval", "Person", "personRef"),
        ("Textual", "Enterprise", "textualCharacteristicValue"),
    ];
    loop {
        let referenced: Vec<Node> = removed
            .iter()
            .copied()
            .filter(|&placeholder| {
                let kind = doc.name(placeholder);
                let key = KINDS.iter().find(|k| k.0 == kind).map_or("id", |k| k.1);
                let id = doc.attr(placeholder, key);
                references
                    .iter()
                    .filter(|reference| reference.1 == kind)
                    .flat_map(|&(from, _, attr)| {
                        doc.find_all(from).into_iter().map(move |node| (node, attr))
                    })
                    .filter(|&(node, _)| !removed.contains(&node) && !upserted(node))
                    .filter(|&(node, _)| !(content_role_replaced && doc.name(node) == "Content"))
                    .filter(|&(node, _)| {
                        doc.name(node) != "Textual"
                            || doc
                                .attr(node, "textualCharacteristicName")
                                .is_some_and(|name| name.ends_with("EnterpriseRef"))
                    })
                    .any(|(node, attr)| doc.attr(node, attr) == id)
            })
            .collect();
        if referenced.is_empty() {
            return removed;
        }
        removed.retain(|node| !referenced.contains(node));
    }
}

/// The edit writing `spec`: it replaces the CadHeader Spec of its name, or
/// joins the Specs, ahead of any CadHeader ChangeRec.
fn spec_edit(doc: &Doc, spec: &ConfigurationSpec) -> Result<Edit> {
    let cad_header = doc
        .root()
        .ok()
        .and_then(|root| doc.child(root, "Ecad"))
        .and_then(|ecad| doc.child(ecad, "CadHeader"))
        .context("IPC-2581 has no CadHeader to record the provenance spec in")?;
    let xml = spec_xml(spec);
    let children = doc.children(cad_header);
    let same_name = children
        .iter()
        .find(|&&child| doc.name(child) == "Spec" && doc.attr(child, "name") == Some(&spec.name));
    let change = children
        .iter()
        .find(|&&child| doc.name(child) == "ChangeRec");
    Ok(match (same_name, change) {
        (Some(&spec), _) => doc.replace(spec, xml),
        (None, Some(&change)) => doc.insert_before(change, xml),
        (None, None) => doc.append_inside(cad_header, xml),
    })
}

fn spec_xml(spec: &ConfigurationSpec) -> String {
    let mut writer = XmlWriter::new();
    writer.start_element("Spec", &[("name", spec.name.as_str())]);
    writer.start_element("General", &[("type", "CONFIGURATION")]);
    for (name, value) in &spec.properties {
        match value {
            PropertyValue::Text(text) => writer.empty_element(
                "Property",
                &[("name", name.as_str()), ("text", text.as_str())],
            ),
            PropertyValue::Number(number) => writer.empty_element(
                "Property",
                &[
                    ("name", name.as_str()),
                    ("value", number.to_string().as_str()),
                ],
            ),
        }
    }
    writer.end_element("General");
    writer.end_element("Spec");
    writer.into_string()
}

/// IPC-2581C restricts these attributes to its enumerations.
fn check_enumerations(input: &ProvenanceInput) -> Result<()> {
    for party in &input.parties {
        let enterprise = &party.enterprise;
        for (attribute, simple_type, value) in [
            (
                "roleFunction",
                "roleFunctionType",
                Some(&party.role.role_function),
            ),
            (
                "codeType",
                "enterpriseCodeType",
                enterprise.code_type.as_ref(),
            ),
            ("country", "isoCodeType", enterprise.country.as_ref()),
        ] {
            if let Some(value) = value
                && !ipc2581::schema_enumeration(simple_type).contains(&value.as_str())
            {
                bail!("{attribute} {value:?} is not an IPC-2581C {simple_type}");
            }
        }
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub fn execute(
    file: &std::path::Path,
    provenance: &std::path::Path,
    output: &std::path::Path,
) -> Result<()> {
    use crate::utils::file as file_utils;

    let json = std::fs::read(provenance)
        .with_context(|| format!("Failed to read provenance: {provenance:?}"))?;
    let input: ProvenanceInput = serde_json::from_slice(&json)
        .with_context(|| format!("Invalid provenance JSON: {provenance:?}"))?;
    let xml = set_provenance(&file_utils::load_ipc_file(file)?, &input)?;
    file_utils::save_ipc_file(output, &xml)?;
    eprintln!("Provenance recorded in {output:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accessors::{IpcAccessor, Provenance};

    /// A KiCad export after `ipc edit bom`: KiCad's placeholder party, a
    /// supplier enterprise a BOM characteristic names and a manufacturer
    /// enterprise the AVL names.
    const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <BomRef name="bom"/>
    <AvlRef name="avl"/>
  </Content>
  <LogisticHeader>
    <Role id="Owner" roleFunction="SENDER"/>
    <Enterprise id="UNKNOWN" code="NONE"/>
    <Enterprise id="Vendor_1" name="Digi-Key" code="NONE"/>
    <Enterprise id="diode-mfr-1" name="Yageo" code="NONE"/>
    <Person name="UNKNOWN" enterpriseRef="UNKNOWN" roleRef="Owner"/>
  </LogisticHeader>
  <HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="KiCad EDA" lastChange="2026-01-01T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="Initial export">
      <SoftwarePackage name="KiCad" revision="10.0.4" vendor="KiCad EDA">
        <Certification certificationStatus="SELFTEST"/>
      </SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Bom name="bom">
    <BomHeader assembly="board" revision="1"><StepRef name="board"/></BomHeader>
    <BomItem OEMDesignNumberRef="r" quantity="1" category="ELECTRICAL">
      <RefDes name="R1" packageRef="R0402" populate="true" layerRef="TOP"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="urn:diode:ipc2581:offer:v1" textualCharacteristicName="SelectedSupplierEnterpriseRef" textualCharacteristicValue="Vendor_1"/>
      </Characteristics>
    </BomItem>
  </Bom>
  <Ecad name="board">
    <CadHeader units="MILLIMETER">
      <Spec name="mask"><General type="MATERIAL"><Property text="LPI"/></General></Spec>
    </CadHeader>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board"><Datum x="0" y="0"/></Step>
    </CadData>
  </Ecad>
  <Avl name="avl">
    <AvlHeader title="parts" source="pcb" author="pcb" datetime="2026-01-01T00:00:00Z" version="1"/>
    <AvlItem OEMDesignNumber="r">
      <AvlVmpn qualified="true" chosen="true"><AvlMpn name="RC0402" rank="1"/><AvlVendor enterpriseRef="diode-mfr-1"/></AvlVmpn>
    </AvlItem>
  </Avl>
</IPC-2581>
"#;

    fn input() -> ProvenanceInput {
        serde_json::from_str(include_str!("testdata/provenance.json")).unwrap()
    }

    fn provenance(xml: &str) -> Provenance {
        IpcAccessor::new(&ipc2581::Ipc2581::parse(xml).unwrap()).provenance()
    }

    fn without_history(xml: &str) -> String {
        let start = xml.find("<HistoryRecord").unwrap();
        let end = xml.find("</HistoryRecord>").unwrap();
        format!("{}{}", &xml[..start], &xml[end..])
    }

    #[test]
    fn records_provenance_that_reads_back_and_validates() {
        ipc2581::validate(XML).unwrap();
        let edited = set_provenance(XML, &input()).unwrap();
        ipc2581::validate(&edited).unwrap();

        let read = provenance(&edited);
        let json = serde_json::to_value(&read).unwrap();
        assert_eq!(read.content_role_ref, "diode-owner");
        // The placeholders go; the enterprises the BOM and AVL name stay.
        let ids = |key: &str, id: &str| -> Vec<String> {
            json[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|entry| entry[id].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids("roles", "id"), ["diode-owner", "diode-sender"]);
        assert_eq!(
            ids("enterprises", "id"),
            ["Vendor_1", "diode-mfr-1", "acme", "diode"]
        );
        assert_eq!(ids("persons", "name"), ["Ada Lovelace", "Diode"]);
        assert_eq!(json["enterprises"][2]["stateProvince"], "MA");
        assert_eq!(json["persons"][0]["roleRef"], "diode-owner");
        assert_eq!(json["persons"][0]["enterpriseRef"], "acme");
        assert_eq!(json["persons"][0]["title"], "Hardware Lead");
        assert_eq!(json["roles"][1]["authority"], "Diode");

        assert_eq!(
            json["bom"],
            serde_json::json!({"assembly": "rover-main", "revision": "1.4.0"})
        );
        assert_eq!(json["history"]["lifecyclePhase"], "PRODUCTION");
        assert_eq!(
            json["history"]["externalConfigurationEntryPoint"],
            "https://app.diode.computer/runs/run_42"
        );
        assert_eq!(json["changes"][0]["personRef"], "Diode");
        assert_eq!(json["changes"][0]["change"], "Provenance updated");
        // The material spec is not configuration.
        assert_eq!(
            json["specs"],
            serde_json::json!([{
                "name": "Diode_Provenance",
                "properties": {
                    "commitSha": "0123abcd",
                    "orderNumber": "ORD-1001",
                    "quantity": 25,
                    "releaseVersion": "1.4.0",
                    "runId": "run_42",
                },
            }])
        );
        assert!(edited.contains(r#"<Property name="quantity" value="25"/>"#));
        assert!(edited.contains(r#"<Property name="runId" text="run_42"/>"#));

        // Again: the same document but for history, and the spec replaced whole.
        assert_eq!(
            without_history(&set_provenance(&edited, &input()).unwrap()),
            without_history(&edited)
        );
        let mut rerun = input();
        rerun
            .spec
            .as_mut()
            .unwrap()
            .properties
            .remove("orderNumber");
        let rerun = set_provenance(&edited, &rerun).unwrap();
        ipc2581::validate(&rerun).unwrap();
        let rerun = provenance(&rerun);
        assert_eq!(rerun.specs.len(), 1);
        assert!(!rerun.specs[0].properties.contains_key("orderNumber"));
        assert_eq!(rerun.changes.len(), 2);
    }

    #[test]
    fn placeholders_earlier_history_names_stay() {
        // `ipc edit population` attributed its change to KiCad's Person.
        let xml = XML.replace(
            "    </FileRevision>\n",
            "    </FileRevision>\n    <ChangeRec datetime=\"2026-01-02T00:00:00Z\" personRef=\"UNKNOWN\" application=\"pcb\" change=\"Population changed\"/>\n",
        );
        let edited = set_provenance(&xml, &input()).unwrap();
        ipc2581::validate(&edited).unwrap();

        let read = provenance(&edited);
        assert_eq!(read.content_role_ref, "diode-owner");
        assert!(read.roles.iter().any(|role| role.id == "Owner"));
        assert!(read.enterprises.iter().any(|e| e.id == "UNKNOWN"));
        assert!(read.persons.iter().any(|p| p.details.name == "UNKNOWN"));
    }

    #[test]
    fn upserts_keep_public_keys_and_new_histories_keep_their_author() {
        // The input reassigns this Person, so the placeholders it named go.
        let person =
            r#"<Person name="Diode" enterpriseRef="UNKNOWN" roleRef="Owner" publicKey="YWJj"/>"#;
        let start = XML.find("<HistoryRecord").unwrap();
        let end = XML.find("</HistoryRecord>").unwrap() + "</HistoryRecord>".len();
        let xml = format!(
            r#"{}<HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="KiCad EDA" lastChange="2026-01-01T00:00:00Z"/>{}"#,
            &XML[..start],
            &XML[end..]
        )
        .replace(r#"<Person name="UNKNOWN" enterpriseRef="UNKNOWN" roleRef="Owner"/>"#, person);
        let edited = set_provenance(&xml, &input()).unwrap();
        ipc2581::validate(&edited).unwrap();

        assert!(edited.contains(r#"<Person name="Diode" enterpriseRef="diode" email="support@diode.computer" roleRef="diode-sender" publicKey="YWJj"/>"#));
        let read = provenance(&edited);
        assert!(!read.roles.iter().any(|role| role.id == "Owner"));
        assert!(!read.enterprises.iter().any(|e| e.id == "UNKNOWN"));
        assert_eq!(read.changes.len(), 1);
        assert_eq!(read.changes[0].person_ref, "Diode");
    }

    #[test]
    fn rejects_provenance_the_schema_or_document_cannot_hold() {
        let mut input = input();
        input.parties[0].role.role_function = "FOUNDER".into();
        let error = set_provenance(XML, &input).unwrap_err().to_string();
        assert_eq!(
            error,
            r#"roleFunction "FOUNDER" is not an IPC-2581C roleFunctionType"#
        );

        let mut input = self::input();
        input.changed_by = "Nobody".into();
        let error = set_provenance(XML, &input).unwrap_err().to_string();
        assert_eq!(error, "changedBy names no Person: Nobody");
    }
}
