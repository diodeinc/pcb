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
}

#[derive(Debug, Clone)]
pub struct Enterprise {
    pub id: Symbol,
    pub code: Symbol,
    pub name: Option<Symbol>,
}

#[derive(Debug, Clone)]
pub struct Person {
    pub name: Symbol,
    pub email: Option<Symbol>,
}

/// History record (file revision history)
#[derive(Debug, Clone)]
pub struct HistoryRecord {
    pub number: u32,
    pub origination: Symbol,
    pub software: Option<Symbol>,
    pub last_change: Symbol,
    pub file_revision: Option<FileRevision>,
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
