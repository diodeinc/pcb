//! Waivers: accepted findings, keyed by their stable ids.
//!
//! A waiver file lets a team ship a known violation without silencing the
//! rule. Waived findings stay in the report — marked, counted, and excluded
//! from the verdict — and waivers that match nothing or have expired are
//! themselves reported so the file cannot rot silently.

use anyhow::Result;
use chrono::NaiveDate;
use serde::Deserialize;

use super::report::Finding;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WaiverFile {
    #[serde(default)]
    pub waiver: Vec<Waiver>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Waiver {
    /// The finding id this waiver accepts, e.g. `dfm-9c41f2ab8d10`.
    pub finding: String,
    pub reason: String,
    /// The waiver is inactive from this date on.
    #[serde(default)]
    pub expires: Option<Expiry>,
}

/// A `YYYY-MM-DD` expiry date, validated at parse.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "String")]
pub(super) struct Expiry(NaiveDate);

impl TryFrom<String> for Expiry {
    type Error = String;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        NaiveDate::parse_from_str(&value, "%Y-%m-%d")
            .map(Self)
            .map_err(|_| format!("expiry '{value}' must be a YYYY-MM-DD date"))
    }
}

impl WaiverFile {
    pub fn parse(source: &str) -> Result<Self> {
        Ok(toml::from_str(source)?)
    }
}

#[derive(Debug, Default)]
pub(super) struct WaiverOutcome {
    pub applied: usize,
    pub expired: Vec<String>,
    pub unmatched: Vec<String>,
}

/// Mark every finding an active waiver names. Expired and unmatched waivers
/// are collected for the report instead of being applied.
pub(super) fn apply(
    findings: &mut [Finding],
    file: &WaiverFile,
    today: NaiveDate,
) -> WaiverOutcome {
    let mut outcome = WaiverOutcome::default();
    for waiver in &file.waiver {
        let expired = waiver.expires.is_some_and(|expires| today >= expires.0);
        let Some(finding) = findings
            .iter_mut()
            .find(|finding| finding.id == waiver.finding)
        else {
            outcome.unmatched.push(waiver.finding.clone());
            continue;
        };
        if expired {
            outcome.expired.push(waiver.finding.clone());
            continue;
        }
        finding.waived = true;
        finding.waiver_reason = Some(waiver.reason.clone());
        outcome.applied += 1;
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates_expiries() {
        let file = WaiverFile::parse(
            r#"
[[waiver]]
finding = "dfm-abc123"
reason = "approved by fab"
expires = "2027-01-01"
"#,
        )
        .unwrap();
        assert_eq!(file.waiver.len(), 1);
        assert!(
            WaiverFile::parse("[[waiver]]\nfinding = \"x\"\nreason = \"y\"\nexpires = \"soon\"\n")
                .is_err()
        );
        assert!(WaiverFile::parse("[[waiver]]\nfinding = \"x\"\n").is_err());
    }
}
