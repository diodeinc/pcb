//! What became of every model a footprint names.

use serde::Serialize;

use crate::models::ModelError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Rendered,
    Hidden,
    /// Left out by the options.
    Filtered,
    Absent,
    Unsupported,
    Failed,
}

impl Status {
    pub fn is_missing(self) -> bool {
        matches!(self, Status::Absent | Status::Unsupported | Status::Failed)
    }
}

impl ModelError {
    pub fn status(&self) -> Status {
        match self {
            ModelError::Absent(_) => Status::Absent,
            ModelError::Unsupported(_) => Status::Unsupported,
            ModelError::Failed(_) | ModelError::Empty => Status::Failed,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelCoverage {
    #[serde(skip)]
    pub footprint: usize,
    pub reference: String,
    pub model: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Coverage {
    pub models: Vec<ModelCoverage>,
}

impl Coverage {
    pub fn incomplete(&self) -> impl Iterator<Item = &ModelCoverage> {
        self.models.iter().filter(|m| m.status.is_missing())
    }

    pub fn is_complete(&self) -> bool {
        self.incomplete().next().is_none()
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({ "complete": self.is_complete(), "models": self.models })
    }
}
