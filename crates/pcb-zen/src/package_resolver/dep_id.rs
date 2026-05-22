use anyhow::{Result, bail};
use semver::Version;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedDepId {
    pub path: String,
    pub lane: String,
}

impl ResolvedDepId {
    pub fn new(path: impl Into<String>, lane: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            lane: lane.into(),
        }
    }

    pub(crate) fn for_version(path: impl Into<String>, version: &Version) -> Self {
        Self::new(path, compatibility_lane(version))
    }

    pub fn indirect_key(&self) -> String {
        format!("{}@{}", self.path, self.lane)
    }
}

pub fn compatibility_lane(version: &Version) -> String {
    if version.major == 0 {
        format!("0.{}", version.minor)
    } else {
        version.major.to_string()
    }
}

pub fn parse_lane_qualified_key(raw: &str) -> Result<ResolvedDepId> {
    let Some((path, lane)) = raw.rsplit_once('@') else {
        bail!(
            "Expected lane-qualified dependency key '<module>@<lane>', got '{}'",
            raw
        );
    };
    if path.is_empty() || lane.is_empty() {
        bail!(
            "Expected lane-qualified dependency key '<module>@<lane>', got '{}'",
            raw
        );
    }
    Ok(ResolvedDepId::new(path, lane))
}
