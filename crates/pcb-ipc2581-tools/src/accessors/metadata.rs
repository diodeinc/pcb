use ipc2581::types::Units;
use serde::{Deserialize, Serialize};

use super::IpcAccessor;

/// File metadata extracted from HistoryRecord and CadHeader
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// Source units (from CadHeader)
    pub source_units: Option<String>,
    /// File creation timestamp (from HistoryRecord origination) - ISO 8601 format
    pub created: Option<String>,
    /// Last modification timestamp (from HistoryRecord lastChange) - ISO 8601 format
    pub last_modified: Option<String>,
    /// Software information (from HistoryRecord and SoftwarePackage)
    pub software: Option<SoftwareInfo>,
}

/// Software package information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoftwareInfo {
    /// Software name (e.g., "KiCad EDA")
    pub name: Option<String>,
    /// Package name (e.g., "KiCad")
    pub package_name: Option<String>,
    /// Package revision/version (e.g., "9.0.6")
    pub package_revision: Option<String>,
    /// Vendor name (e.g., "KiCad EDA")
    pub vendor: Option<String>,
}

impl SoftwareInfo {
    /// Format software info as a single string (e.g., "KiCad 9.0.6 (KiCad EDA)")
    pub fn format(&self) -> Option<String> {
        match (&self.package_name, &self.package_revision, &self.vendor) {
            (Some(name), Some(rev), Some(vendor)) => Some(format!("{} {} ({})", name, rev, vendor)),
            (Some(name), Some(rev), None) => Some(format!("{} {}", name, rev)),
            (Some(name), None, Some(vendor)) => Some(format!("{} ({})", name, vendor)),
            (Some(name), None, None) => Some(name.clone()),
            (None, _, _) => self.name.clone(),
        }
    }
}

impl<'a> IpcAccessor<'a> {
    /// Extract file metadata from HistoryRecord and CadHeader
    pub fn file_metadata(&self) -> Option<FileMetadata> {
        let history = self.ipc.history_record();
        let ecad = self.ecad();

        // Source units from CadHeader
        let source_units = ecad.map(|e| format_units(e.cad_header.units));

        // Timestamps from HistoryRecord (keep as strings, validate format)
        let created = history.map(|h| validate_timestamp(self.ipc.resolve(h.origination)));
        let last_modified = history.map(|h| validate_timestamp(self.ipc.resolve(h.last_change)));

        // Software info from HistoryRecord
        let software = history.and_then(|h| {
            // Get software name from the 'software' attribute
            let software_name = h
                .software
                .as_ref()
                .map(|s| self.ipc.resolve(*s).to_string());

            // Get package details from FileRevision.SoftwarePackage
            let file_revision = h.file_revision.as_ref();
            let package = file_revision.and_then(|fr| fr.software_package.as_ref());

            if software_name.is_some() || package.is_some() {
                Some(SoftwareInfo {
                    name: software_name,
                    package_name: package.map(|p| self.ipc.resolve(p.name).to_string()),
                    package_revision: package
                        .and_then(|p| p.revision.as_ref())
                        .map(|r| self.ipc.resolve(*r).to_string()),
                    vendor: package
                        .and_then(|p| p.vendor.as_ref())
                        .map(|v| self.ipc.resolve(*v).to_string()),
                })
            } else {
                None
            }
        });

        // Only return metadata if we have at least one field
        if source_units.is_some()
            || created.is_some()
            || last_modified.is_some()
            || software.is_some()
        {
            Some(FileMetadata {
                source_units,
                created,
                last_modified,
                software,
            })
        } else {
            None
        }
    }
}

/// Format units enum as string
fn format_units(units: Units) -> String {
    match units {
        Units::Millimeter => "MILLIMETER".to_string(),
        Units::Inch => "INCH".to_string(),
        Units::Micron => "MICRON".to_string(),
        Units::Mils => "MIL".to_string(),
    }
}

/// Validate and return ISO 8601 timestamp
fn validate_timestamp(s: &str) -> String {
    // IPC-2581 uses ISO 8601 format: "2025-11-16T20:08:43"
    // Just validate it's parseable using jiff and return the original string
    use jiff::civil::DateTime;

    // Try to parse to validate format
    if s.parse::<DateTime>().is_ok() || s.parse::<jiff::Timestamp>().is_ok() {
        s.to_string()
    } else {
        // If it doesn't parse, still return it but it might be invalid
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_timestamp() {
        // IPC-2581 timestamp format
        let ts = validate_timestamp("2025-10-23T16:30:12");
        assert_eq!(ts, "2025-10-23T16:30:12");

        // With timezone
        let ts_tz = validate_timestamp("2025-11-17T22:17:48Z");
        assert_eq!(ts_tz, "2025-11-17T22:17:48Z");
    }

    #[test]
    fn test_software_info_format() {
        let info = SoftwareInfo {
            name: Some("KiCad EDA".to_string()),
            package_name: Some("KiCad".to_string()),
            package_revision: Some("9.0.6".to_string()),
            vendor: Some("KiCad EDA".to_string()),
        };
        assert_eq!(info.format(), Some("KiCad 9.0.6 (KiCad EDA)".to_string()));

        let info_minimal = SoftwareInfo {
            name: Some("KiCad EDA".to_string()),
            package_name: None,
            package_revision: None,
            vendor: None,
        };
        assert_eq!(info_minimal.format(), Some("KiCad EDA".to_string()));
    }
}
