use anyhow::{Result, bail};
use semver::Version;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ResolvedDepId {
    pub(crate) path: String,
    pub(crate) lane: String,
}

impl ResolvedDepId {
    pub(crate) fn new(path: impl Into<String>, lane: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            lane: lane.into(),
        }
    }

    pub(crate) fn for_version(path: impl Into<String>, version: &Version) -> Self {
        Self::new(path, compatibility_lane(version))
    }

    pub(crate) fn indirect_key(&self) -> String {
        format!("{}@{}", self.path, self.lane)
    }
}

pub(crate) fn compatibility_lane(version: &Version) -> String {
    if version.major == 0 {
        format!("0.{}", version.minor)
    } else {
        version.major.to_string()
    }
}

pub(crate) fn parse_lane_qualified_key(raw: &str) -> Result<ResolvedDepId> {
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

#[cfg(test)]
mod tests {
    use semver::Version;

    use super::*;

    #[test]
    fn compatibility_lane_uses_minor_for_v0_and_major_for_v1_plus() {
        assert_eq!(compatibility_lane(&Version::parse("0.1.7").unwrap()), "0.1");
        assert_eq!(compatibility_lane(&Version::parse("0.8.3").unwrap()), "0.8");
        assert_eq!(compatibility_lane(&Version::parse("1.2.3").unwrap()), "1");
        assert_eq!(compatibility_lane(&Version::parse("2.0.0").unwrap()), "2");
    }

    #[test]
    fn parse_lane_qualified_key_parses_path_and_lane() {
        let parsed = parse_lane_qualified_key("github.com/acme/foo@0.8").unwrap();
        assert_eq!(parsed.path, "github.com/acme/foo");
        assert_eq!(parsed.lane, "0.8");
    }
}
