//! A production run's provenance as the file records it: the parties of its
//! LogisticHeader, its BOM header, its HistoryRecord and the named
//! `General type="CONFIGURATION"` specs of its CadHeader.
//!
//! The same types are the input of `ipc edit provenance`. Field names are the
//! IPC-2581 attribute names.

use std::collections::BTreeMap;

use ipc2581::{Ipc2581, Symbol};
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoleInfo {
    pub id: String,
    pub role_function: String,
    pub description: Option<String>,
    pub authority: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnterpriseInfo {
    pub id: String,
    pub code: String,
    pub name: Option<String>,
    pub code_type: Option<String>,
    pub address1: Option<String>,
    pub address2: Option<String>,
    pub city: Option<String>,
    pub state_province: Option<String>,
    pub country: Option<String>,
    pub postal_code: Option<String>,
    pub phone: Option<String>,
    pub fax: Option<String>,
    pub email: Option<String>,
    pub url: Option<String>,
}

/// A Person's own attributes; its enterprise and role come from its party.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PersonDetails {
    pub name: String,
    pub title: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub fax: Option<String>,
    pub mailstop: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonInfo {
    #[serde(flatten)]
    pub details: PersonDetails,
    pub enterprise_ref: String,
    pub role_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BomHeaderInfo {
    pub assembly: String,
    pub revision: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryInfo {
    pub lifecycle_phase: Option<String>,
    pub external_configuration_entry_point: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeInfo {
    pub datetime: String,
    pub person_ref: String,
    pub application: String,
    pub change: String,
    pub approvals: Vec<ApprovalInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalInfo {
    pub datetime: String,
    pub person_ref: String,
}

/// A named CadHeader Spec of `General type="CONFIGURATION"` properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationSpec {
    pub name: String,
    pub properties: BTreeMap<String, PropertyValue>,
}

/// A configuration property: `Property@text` or `Property@value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PropertyValue {
    Text(String),
    Number(serde_json::Number),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub content_role_ref: String,
    pub roles: Vec<RoleInfo>,
    pub enterprises: Vec<EnterpriseInfo>,
    pub persons: Vec<PersonInfo>,
    pub bom: Option<BomHeaderInfo>,
    pub history: Option<HistoryInfo>,
    pub changes: Vec<ChangeInfo>,
    pub specs: Vec<ConfigurationSpec>,
}

impl<'a> IpcAccessor<'a> {
    /// The file's provenance, read from its typed model.
    pub fn provenance(&self) -> Provenance {
        let ipc = self.ipc;
        let s = |symbol: Symbol| ipc.resolve(symbol).to_string();
        let o = |symbol: Option<Symbol>| symbol.map(s);
        let header = ipc.logistic_header();
        let history = ipc.history_record();

        let mut specs: Vec<_> = self
            .ecad()
            .into_iter()
            .flat_map(|ecad| ecad.cad_header.specs.values())
            .filter_map(|spec| configuration_spec(ipc, spec))
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));

        Provenance {
            content_role_ref: s(ipc.content().role_ref),
            roles: header
                .into_iter()
                .flat_map(|header| &header.roles)
                .map(|role| RoleInfo {
                    id: s(role.id),
                    role_function: s(role.role_function),
                    description: o(role.description),
                    authority: o(role.authority),
                })
                .collect(),
            enterprises: header
                .into_iter()
                .flat_map(|header| &header.enterprises)
                .map(|e| EnterpriseInfo {
                    id: s(e.id),
                    code: s(e.code),
                    name: o(e.name),
                    code_type: o(e.code_type),
                    address1: o(e.address1),
                    address2: o(e.address2),
                    city: o(e.city),
                    state_province: o(e.state_province),
                    country: o(e.country),
                    postal_code: o(e.postal_code),
                    phone: o(e.phone),
                    fax: o(e.fax),
                    email: o(e.email),
                    url: o(e.url),
                })
                .collect(),
            persons: header
                .into_iter()
                .flat_map(|header| &header.persons)
                .map(|p| PersonInfo {
                    details: PersonDetails {
                        name: s(p.name),
                        title: o(p.title),
                        email: o(p.email),
                        phone: o(p.phone),
                        fax: o(p.fax),
                        mailstop: o(p.mailstop),
                    },
                    enterprise_ref: s(p.enterprise_ref),
                    role_ref: s(p.role_ref),
                })
                .collect(),
            bom: ipc
                .bom()
                .and_then(|bom| bom.header.as_ref())
                .map(|header| BomHeaderInfo {
                    assembly: s(header.assembly),
                    revision: s(header.revision),
                }),
            history: history.map(|record| HistoryInfo {
                lifecycle_phase: o(record.lifecycle_phase),
                external_configuration_entry_point: o(record.external_configuration_entry_point),
            }),
            changes: history
                .into_iter()
                .flat_map(|record| &record.change_recs)
                .map(|change| ChangeInfo {
                    datetime: s(change.datetime),
                    person_ref: s(change.person_ref),
                    application: s(change.application),
                    change: s(change.change),
                    approvals: change
                        .approvals
                        .iter()
                        .map(|approval| ApprovalInfo {
                            datetime: s(approval.datetime),
                            person_ref: s(approval.person_ref),
                        })
                        .collect(),
                })
                .collect(),
            specs,
        }
    }
}

/// The named properties of a spec's `General type="CONFIGURATION"` items, or
/// `None` when it has none.
fn configuration_spec(ipc: &Ipc2581, spec: &ipc2581::types::Spec) -> Option<ConfigurationSpec> {
    let items: Vec<_> = spec
        .items
        .iter()
        .filter(|item| {
            item.kind == ipc2581::types::SpecItemKind::General
                && item.item_type.map(|t| ipc.resolve(t)) == Some("CONFIGURATION")
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    let properties = items
        .into_iter()
        .flat_map(|item| &item.properties)
        .filter_map(|property| {
            let name = ipc.resolve(property.name?).to_string();
            let value = match (property.text, property.value) {
                (Some(text), _) => PropertyValue::Text(ipc.resolve(text).to_string()),
                (None, Some(value)) => PropertyValue::Number(json_number(value)?),
                (None, None) => return None,
            };
            Some((name, value))
        })
        .collect();
    Some(ConfigurationSpec {
        name: ipc.resolve(spec.name).to_string(),
        properties,
    })
}

/// A whole `value` reads back as the integer it was written as.
fn json_number(value: f64) -> Option<serde_json::Number> {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        Some((value as i64).into())
    } else {
        serde_json::Number::from_f64(value)
    }
}
