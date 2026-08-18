use pcb_ir::geom::{BBox, Point};
use serde::Serialize;

use super::pdk::{Length, Pdk};

pub const REPORT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct DfmReport {
    pub schema_version: u32,
    pub generated_at: String,
    pub verdict: Verdict,
    pub tool: ToolIdentity,
    pub input: FileIdentity,
    pub pdk: PdkIdentity,
    pub layout_target: &'static str,
    pub coordinate_system: CoordinateSystem,
    pub summary: Summary,
    pub rules: Vec<RuleResult>,
    pub findings: Vec<Finding>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
}

#[derive(Debug, Serialize)]
pub struct ToolIdentity {
    pub name: &'static str,
    pub version: &'static str,
}

#[derive(Debug, Serialize)]
pub struct FileIdentity {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct PdkIdentity {
    pub id: String,
    pub name: String,
    pub revision: String,
    pub manufacturer: Option<String>,
    pub process: Option<String>,
    pub path: String,
    pub sha256: String,
}

impl PdkIdentity {
    pub fn from_pdk(pdk: &Pdk, path: String, sha256: String) -> Self {
        Self {
            id: pdk.pdk.id.clone(),
            name: pdk.pdk.name.clone(),
            revision: pdk.pdk.revision.clone(),
            manufacturer: pdk.pdk.manufacturer.clone(),
            process: pdk.pdk.process.clone(),
            path,
            sha256,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CoordinateSystem {
    pub unit: &'static str,
    pub axes: &'static str,
    pub origin: &'static str,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub rules_configured: usize,
    pub rules_passed: usize,
    pub rules_failed: usize,
    pub rules_skipped: usize,
    pub findings: usize,
}

#[derive(Debug, Serialize)]
pub struct RuleResult {
    pub id: &'static str,
    pub title: &'static str,
    pub status: RuleStatus,
    pub limit: RuleLimit,
    pub checked_entities: usize,
    pub finding_count: usize,
    pub skip_reason: Option<String>,
}

impl RuleResult {
    pub fn new(id: &'static str, title: &'static str, length: &Length) -> Self {
        Self {
            id,
            title,
            status: RuleStatus::Pass,
            limit: RuleLimit::from_length(length),
            checked_entities: 0,
            finding_count: 0,
            skip_reason: None,
        }
    }

    pub fn finish(&mut self, finding_count: usize) {
        self.finding_count = finding_count;
        self.status = if finding_count == 0 {
            RuleStatus::Pass
        } else {
            RuleStatus::Fail
        };
    }

    pub fn skip(&mut self, reason: impl Into<String>) {
        self.status = RuleStatus::Skipped;
        self.skip_reason = Some(reason.into());
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    Pass,
    Fail,
    Skipped,
}

#[derive(Debug, Serialize)]
pub struct RuleLimit {
    pub pdk_value: String,
    pub normalized_value: f64,
    pub normalized_unit: &'static str,
}

impl RuleLimit {
    fn from_length(length: &Length) -> Self {
        Self {
            pdk_value: length.original().to_owned(),
            normalized_value: length.millimeters(),
            normalized_unit: "mm",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub id: String,
    pub rule_id: &'static str,
    pub severity: Severity,
    pub title: String,
    pub message: String,
    pub measurements: Vec<Measurement>,
    pub location: Location,
    pub layers: Vec<LayerRef>,
    pub subjects: Vec<Subject>,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
}

#[derive(Debug, Serialize)]
pub struct Measurement {
    pub quantity: &'static str,
    pub actual: f64,
    pub required: f64,
    pub margin: f64,
    pub unit: &'static str,
    pub comparison: &'static str,
    pub method: &'static str,
}

impl Measurement {
    pub fn minimum(
        quantity: &'static str,
        actual: f64,
        required: f64,
        method: &'static str,
    ) -> Self {
        Self {
            quantity,
            actual,
            required,
            margin: actual - required,
            unit: "mm",
            comparison: "actual >= required",
            method,
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Location {
    pub point: Option<ReportPoint>,
    pub bounding_box: Option<ReportBBox>,
    pub witnesses: Vec<Witness>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Witness {
    pub role: &'static str,
    pub point: ReportPoint,
}

impl Witness {
    pub fn new(role: &'static str, point: Point) -> Self {
        Self {
            role,
            point: point.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReportPoint {
    pub x: f64,
    pub y: f64,
}

impl From<Point> for ReportPoint {
    fn from(point: Point) -> Self {
        Self {
            x: point.x,
            y: point.y,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ReportBBox {
    pub min: ReportPoint,
    pub max: ReportPoint,
}

impl From<BBox> for ReportBBox {
    fn from(bbox: BBox) -> Self {
        Self {
            min: bbox.min.into(),
            max: bbox.max.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LayerRef {
    pub name: String,
    pub function: String,
}

#[derive(Debug, Default, Serialize)]
pub struct Subject {
    pub role: &'static str,
    pub kind: &'static str,
    pub name: Option<String>,
    pub reference_designator: Option<String>,
    pub footprint: Option<String>,
    pub pin: Option<String>,
    pub net: Option<String>,
    pub padstack_ref: Option<String>,
    pub source: Option<SourceLocator>,
}

#[derive(Debug, Serialize)]
pub struct SourceLocator {
    pub step: Option<String>,
    pub layer: Option<String>,
    pub set_index: Option<u32>,
    pub feature_index: Option<u32>,
    pub instance_index: Option<u32>,
}

#[derive(Debug, Default, Serialize)]
pub struct Evidence {
    pub role: &'static str,
    pub kind: &'static str,
    pub center: Option<ReportPoint>,
    pub diameter: Option<f64>,
    pub start: Option<ReportPoint>,
    pub end: Option<ReportPoint>,
    pub bounding_box: Option<ReportBBox>,
}

impl Evidence {
    pub fn circle(role: &'static str, center: Point, diameter: f64) -> Self {
        Self {
            role,
            kind: "circle",
            center: Some(center.into()),
            diameter: Some(diameter),
            ..Self::default()
        }
    }

    pub fn segment(role: &'static str, start: Point, end: Point) -> Self {
        Self {
            role,
            kind: "segment",
            start: Some(start.into()),
            end: Some(end.into()),
            ..Self::default()
        }
    }

    pub fn bounds(role: &'static str, bounding_box: BBox) -> Self {
        Self {
            role,
            kind: "bounds",
            bounding_box: Some(bounding_box.into()),
            ..Self::default()
        }
    }
}
