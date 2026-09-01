use pcb_ir::geom::region::ContourSet;
use pcb_ir::geom::{BBox, Point};
use serde::Serialize;

use super::pdk::Pdk;
use super::rules::{LimitValue, Rule};

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
    /// The actual checked frame and its hierarchy, not a second layout model.
    pub layout: LayoutContext,
    pub waivers: Option<WaiversApplied>,
    pub summary: Summary,
    pub rules: Vec<RuleResult>,
    pub findings: Vec<Finding>,
    /// Full native artwork for external diagnostic viewers.
    pub scene: Scene,
}

#[derive(Debug, Serialize)]
pub struct Scene {
    pub schema_version: u32,
    /// Full checked layout extent in the report's millimeter, Y-up frame.
    pub bounds: ReportBBox,
    pub passes: Vec<ScenePass>,
}

#[derive(Debug, Serialize)]
pub struct ScenePass {
    pub label: String,
    pub feature: &'static str,
    pub layer: Option<String>,
    pub color: &'static str,
    /// One full vector image in world coordinates. The SVG root applies the
    /// usual Y display flip; finding sites never crop or duplicate this image.
    pub svg: String,
}

/// The waiver file applied to this run and what came of every entry.
#[derive(Debug, Serialize)]
pub struct WaiversApplied {
    pub path: String,
    pub sha256: String,
    pub applied: usize,
    /// Waived finding ids whose waiver has expired; they count as findings.
    pub expired: Vec<String>,
    /// Waiver entries naming no finding in this run — stale or mistyped.
    pub unmatched: Vec<String>,
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

#[derive(Debug, Clone, Serialize)]
pub struct FileIdentity {
    pub path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

impl FileIdentity {
    /// Identify the original input bytes, before decoding or decompression.
    /// `path` is a caller-provided label and is never opened by this method.
    pub fn new(path: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            path: path.into(),
            sha256: super::sha256(bytes),
            size_bytes: bytes.len() as u64,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PdkIdentity {
    pub id: String,
    pub name: String,
    pub revision: String,
    pub manufacturer: Option<String>,
    pub process: Option<String>,
    pub profile: String,
    pub profile_name: String,
    pub profile_description: Option<String>,
    pub profile_status: &'static str,
    pub performance_class: Option<u8>,
    pub producibility_level: Option<&'static str>,
    pub technologies: Vec<&'static str>,
    pub coverage: Vec<String>,
    pub support: PdkProfileSupport,
    pub defaults: PdkProfileDefaults,
    pub profile_source: Option<PdkSourceReference>,
    pub path: String,
    pub sha256: String,
    /// Exact resolved UTF-8 PDK TOML used by this check, without reserialization.
    pub source: String,
}

#[derive(Debug, Serialize)]
pub struct PdkProfileSupport {
    pub copper_layers: Option<PdkCountRange>,
}

#[derive(Debug, Serialize)]
pub struct PdkCountRange {
    pub exact: Option<u32>,
    pub minimum: Option<u32>,
    pub maximum: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct PdkProfileDefaults {
    pub material: Option<String>,
    pub board_thickness: Option<String>,
    pub outer_copper_weight: Option<String>,
    pub inner_copper_weight: Option<String>,
    pub soldermask_color: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PdkSourceReference {
    pub id: String,
    pub title: String,
    pub url: String,
    pub revision: Option<String>,
    pub accessed: Option<String>,
    pub note: Option<String>,
}

impl PdkIdentity {
    pub(super) fn from_pdk(
        pdk: &Pdk,
        selected_profile: Option<&str>,
        path: String,
        sha256: String,
        source: String,
    ) -> Self {
        let (profile, definition) = pdk
            .selected_profile(selected_profile)
            .expect("selected profile was validated while lowering rules");
        let profile_source = definition.source.as_ref().and_then(|id| {
            pdk.sources.get(id).map(|source| PdkSourceReference {
                id: id.clone(),
                title: source.title.clone(),
                url: source.url.clone(),
                revision: source.revision.clone(),
                accessed: source.accessed.clone(),
                note: source.note.clone(),
            })
        });
        Self {
            id: pdk.pdk.id.clone(),
            name: pdk.pdk.name.clone(),
            revision: pdk.pdk.revision.clone(),
            manufacturer: pdk.pdk.manufacturer.clone(),
            process: pdk.pdk.process.clone(),
            profile: profile.to_owned(),
            profile_name: definition.name.clone(),
            profile_description: definition.description.clone(),
            profile_status: definition.status.label(),
            performance_class: definition.performance_class,
            producibility_level: definition.producibility_level.map(|level| level.label()),
            technologies: definition
                .technologies
                .iter()
                .map(|technology| technology.label())
                .collect(),
            coverage: definition.coverage.clone(),
            support: PdkProfileSupport {
                copper_layers: definition.support.copper_layers.as_ref().map(|range| {
                    PdkCountRange {
                        exact: range.exact,
                        minimum: range.minimum,
                        maximum: range.maximum,
                    }
                }),
            },
            defaults: PdkProfileDefaults {
                material: definition.defaults.material.clone(),
                board_thickness: definition
                    .defaults
                    .board_thickness
                    .as_ref()
                    .map(|value| value.original().to_owned()),
                outer_copper_weight: definition
                    .defaults
                    .outer_copper_weight
                    .as_ref()
                    .map(|value| value.original().to_owned()),
                inner_copper_weight: definition
                    .defaults
                    .inner_copper_weight
                    .as_ref()
                    .map(|value| value.original().to_owned()),
                soldermask_color: definition.defaults.soldermask_color.clone(),
            },
            profile_source,
            path,
            sha256,
            source,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CoordinateSystem {
    pub unit: &'static str,
    pub axes: &'static str,
    pub origin: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutContext {
    pub kind: &'static str,
    pub selected_step: Option<String>,
    pub coordinate_frame: &'static str,
    pub bounding_box: Option<ReportBBox>,
    pub instances: Vec<LayoutOccurrence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LayoutOccurrence {
    pub index: u32,
    pub parent_index: Option<u32>,
    pub step: String,
    pub kind: &'static str,
    pub purpose: &'static str,
    /// Child-local to checked-root affine matrix [a, b, c, d, tx, ty].
    pub transform: [f64; 6],
    pub bounding_box: Option<ReportBBox>,
    pub repeat_index_x: u32,
    pub repeat_index_y: u32,
}

/// Explicit rule-to-view mapping. Features are semantic composed passes, never
/// source primitive filters that could change the material being measured.
#[derive(Debug, Clone, Serialize)]
pub struct ViewRecipe {
    pub kind: &'static str,
    pub title: &'static str,
    pub spatial: bool,
    pub features: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub rules_configured: usize,
    pub rules_passed: usize,
    pub rules_warned: usize,
    pub rules_failed: usize,
    pub rules_skipped: usize,
    pub findings: usize,
    /// Unwaived error-severity findings; the verdict fails on these alone.
    pub errors: usize,
    /// Unwaived warning-severity findings.
    pub warnings: usize,
    pub waived: usize,
}

#[derive(Debug, Serialize)]
pub struct RuleResult {
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub status: RuleStatus,
    pub limit: RuleLimit,
    /// Whether values below or above the limit violate this rule.
    pub comparison: &'static str,
    /// What one `checked` unit is, e.g. `hole` or `copper_layer`.
    pub subject: &'static str,
    /// The quantity every finding of this rule measures.
    pub quantity: &'static str,
    /// How the quantity is measured.
    pub method: &'static str,
    /// Measurements evaluated against the limit.
    pub checked: usize,
    pub finding_count: usize,
    pub waived_count: usize,
    pub skip_reason: Option<String>,
    pub view: ViewRecipe,
    pub tier: &'static str,
}

impl RuleResult {
    pub(super) fn new(rule: &Rule) -> Self {
        let semantics = rule.kind.semantics();
        Self {
            id: rule.id.clone(),
            title: rule.title.clone(),
            severity: rule.severity,
            status: RuleStatus::Pass,
            limit: RuleLimit::from_value(&rule.limit),
            comparison: rule.comparison.label(),
            subject: semantics.subject,
            quantity: semantics.quantity,
            method: semantics.method,
            checked: 0,
            finding_count: 0,
            waived_count: 0,
            skip_reason: None,
            view: rule.kind.view_recipe(),
            tier: if rule.severity == Severity::Warning {
                "preferred"
            } else {
                "required"
            },
        }
    }

    /// Settle the rule's status from its finding counts: unwaived findings
    /// carry the rule's severity, a fully waived or clean rule passes.
    pub fn finish(&mut self, finding_count: usize, waived_count: usize) {
        self.finding_count = finding_count;
        self.waived_count = waived_count;
        self.status = if finding_count == waived_count {
            RuleStatus::Pass
        } else if self.severity == Severity::Warning {
            RuleStatus::Warning
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
    Warning,
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
    fn from_value(value: &LimitValue) -> Self {
        match value {
            LimitValue::Length(length) => Self {
                pdk_value: length.original().to_owned(),
                normalized_value: length.millimeters(),
                normalized_unit: "mm",
            },
            LimitValue::Count(count) => Self {
                pdk_value: count.to_string(),
                normalized_value: f64::from(*count),
                normalized_unit: "layers",
            },
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub id: String,
    pub rule_id: String,
    pub severity: Severity,
    pub waived: bool,
    pub waiver_reason: Option<String>,
    pub title: String,
    pub message: String,
    pub measurement: Measurement,
    pub location: Location,
    pub layers: Vec<LayerRef>,
    pub subjects: Vec<Subject>,
    pub evidence: Vec<Evidence>,
    /// Check-owned connected regions/layers. The finding remains the waiver unit.
    pub sites: Vec<Site>,
    /// Presentation-only identity for proven equivalent repeated causes.
    pub group_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementKind {
    Diameter,
    NominalWidth,
    InscribedWidth,
    Clearance,
    RadialEnclosure,
    Overlap,
    MissingCopper,
}

#[derive(Debug, Clone, Serialize)]
pub struct Site {
    pub id: String,
    pub measurement: Measurement,
    pub measurement_kind: MeasurementKind,
    pub uncertainty_mm: f64,
    pub witnesses: Vec<Witness>,
    /// Check-owned region of interest in the checked frame. The viewer adds
    /// camera padding; contextual source bounds may extend beyond this region.
    pub bounding_box: ReportBBox,
    pub layers: Vec<LayerRef>,
    pub subjects: Vec<Subject>,
    pub evidence: Vec<Evidence>,
    /// Explains candidate geometry, assumed spans, or special measurement states.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
}

/// The one measurement that gates a finding. The quantity, method, unit, and
/// comparison live on the finding's rule.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum Measurement {
    Distance {
        actual_mm: f64,
        required_mm: f64,
        margin_mm: f64,
    },
    Count {
        actual_count: u32,
        required_count: u32,
        margin_count: i64,
    },
}

impl Measurement {
    pub fn minimum_distance(actual_mm: f64, required_mm: f64) -> Self {
        Self::Distance {
            actual_mm,
            required_mm,
            margin_mm: actual_mm - required_mm,
        }
    }

    pub fn minimum_count(actual_count: u32, required_count: u32) -> Self {
        Self::Count {
            actual_count,
            required_count,
            margin_count: i64::from(actual_count) - i64::from(required_count),
        }
    }

    pub fn maximum_count(actual_count: u32, required_count: u32) -> Self {
        Self::Count {
            actual_count,
            required_count,
            margin_count: i64::from(required_count) - i64::from(actual_count),
        }
    }

    #[cfg(test)]
    pub fn actual_mm(&self) -> Option<f64> {
        match self {
            Self::Distance { actual_mm, .. } => Some(*actual_mm),
            Self::Count { .. } => None,
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

impl ReportBBox {
    pub fn as_bbox(self) -> BBox {
        BBox::new(
            Point::new(self.min.x, self.min.y),
            Point::new(self.max.x, self.max.y),
        )
    }
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
    /// `top`, `inner`, or `bottom` where the file or stackup determines it.
    pub side: Option<&'static str>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Subject {
    pub role: &'static str,
    pub kind: &'static str,
    pub name: Option<String>,
    pub reference_designator: Option<String>,
    pub pin: Option<String>,
    pub net: Option<String>,
    pub padstack_ref: Option<String>,
    pub source: Option<SourceLocator>,
    /// Definition-local source coordinates plus its physical occurrence. `source`
    /// retains the historical flattened locator for compatibility.
    pub provenance: Option<SourceLocator>,
    pub drill_span: Option<DrillSpan>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DrillSpan {
    pub first_copper_index: u16,
    pub last_copper_index: u16,
    pub interpretation: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceLocator {
    pub step: Option<String>,
    pub layer: Option<String>,
    pub set_index: Option<u32>,
    pub feature_index: Option<u32>,
    pub instance_index: Option<u32>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Evidence {
    pub role: &'static str,
    pub kind: &'static str,
    pub center: Option<ReportPoint>,
    pub diameter: Option<f64>,
    pub start: Option<ReportPoint>,
    pub end: Option<ReportPoint>,
    pub bounding_box: Option<ReportBBox>,
    /// Closed rings for `region`, open point sequences for `path`. Region rings
    /// use the same nonzero winding as the checked composed material.
    pub paths: Vec<Vec<ReportPoint>>,
    /// Optional native construction for display. The measured paths above,
    /// witness points, and uncertainty remain the check's authoritative data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<EvidenceDisplay>,
}

/// Compact display geometry in the report's millimeter, Y-up frame. These
/// constructions retain source curves without fitting or smoothing measured
/// polygons, and never participate in measurements or diagnostic identity.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceDisplay {
    /// Independently filled native SVG paths, union-composited by the viewer.
    Path {
        paths: Vec<String>,
        fill_rule: &'static str,
    },
    /// A physical-width round-capped, round-joined stroke of these paths.
    RoundStroke {
        paths: Vec<Vec<ReportPoint>>,
        width_mm: f64,
    },
    /// Required circular copper minus the named native copper layer image.
    CircleMinusLayer {
        center: ReportPoint,
        diameter: f64,
        layer: String,
    },
    CircleIntersection {
        first: DisplayCircle,
        second: DisplayCircle,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DisplayCircle {
    pub center: ReportPoint,
    pub diameter: f64,
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

    pub fn region(role: &'static str, region: &ContourSet) -> Self {
        Self {
            role,
            kind: "region",
            bounding_box: (!region.is_empty()).then(|| region.bbox.into()),
            paths: region
                .rings
                .iter()
                .map(|ring| ring.iter().map(|&[x, y]| ReportPoint { x, y }).collect())
                .collect(),
            ..Self::default()
        }
    }
}
