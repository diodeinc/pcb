use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// String wrapper with natural ordering (C1 < C2 < C10)
/// Automatically maintains natural sort order in BTreeSet/BTreeMap
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NaturalString(String);

impl NaturalString {
    pub fn new(s: String) -> Self {
        Self(s)
    }
}

impl From<String> for NaturalString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for NaturalString {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for NaturalString {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for NaturalString {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<String> for NaturalString {
    fn borrow(&self) -> &String {
        &self.0
    }
}

impl std::fmt::Display for NaturalString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl PartialOrd for NaturalString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NaturalString {
    fn cmp(&self, other: &Self) -> Ordering {
        natord::compare(&self.0, &other.0)
    }
}
