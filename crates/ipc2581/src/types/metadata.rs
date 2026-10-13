use crate::Symbol;

/// Logistic header (order and supply chain information)
#[derive(Debug, Clone)]
pub struct LogisticHeader {
    pub roles: Vec<Role>,
    pub enterprises: Vec<Enterprise>,
    pub persons: Vec<Person>,
}

#[derive(Debug, Clone)]
pub struct Role {
    pub id: Symbol,
    pub role_function: Symbol,
    pub description: Option<Symbol>,
    pub authority: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub struct Enterprise {
    pub id: Symbol,
    pub code: Symbol,
    pub name: Option<Symbol>,
    pub code_type: Option<Symbol>,
    pub address1: Option<Symbol>,
    pub address2: Option<Symbol>,
    pub city: Option<Symbol>,
    pub state_province: Option<Symbol>,
    pub country: Option<Symbol>,
    pub postal_code: Option<Symbol>,
    pub phone: Option<Symbol>,
    pub fax: Option<Symbol>,
    pub email: Option<Symbol>,
    pub url: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub struct Person {
    pub name: Symbol,
    pub enterprise_ref: Symbol,
    pub role_ref: Symbol,
    pub title: Option<Symbol>,
    pub email: Option<Symbol>,
    pub phone: Option<Symbol>,
    pub fax: Option<Symbol>,
    pub mailstop: Option<Symbol>,
}

/// History record (file revision history)
#[derive(Debug, Clone)]
pub struct HistoryRecord {
    pub number: u32,
    pub origination: Symbol,
    pub software: Option<Symbol>,
    pub last_change: Symbol,
    pub lifecycle_phase: Option<Symbol>,
    pub external_configuration_entry_point: Option<Symbol>,
    pub file_revision: Option<FileRevision>,
    pub change_recs: Vec<ChangeRec>,
}

/// One change to the file after its FileRevision.
#[derive(Debug, Clone)]
pub struct ChangeRec {
    pub datetime: Symbol,
    pub person_ref: Symbol,
    pub application: Symbol,
    pub change: Symbol,
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone)]
pub struct Approval {
    pub datetime: Symbol,
    pub person_ref: Symbol,
}

#[derive(Debug, Clone)]
pub struct FileRevision {
    pub file_revision: Symbol,
    pub comment: Option<Symbol>,
    pub software_package: Option<SoftwarePackage>,
}

#[derive(Debug, Clone)]
pub struct SoftwarePackage {
    pub name: Symbol,
    pub revision: Option<Symbol>,
    pub vendor: Option<Symbol>,
}
