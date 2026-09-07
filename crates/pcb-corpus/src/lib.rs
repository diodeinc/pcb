//! Versioned, source-independent geometry replay. No import occurs during replay.
use pcb_ir::geom::{
    AccuracyError, ContourSet, FillRule, GeometryAccuracy, Resolution, region::Ring,
};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
/// Numerical budget relative to the stored polygon inputs, not their source curves.
pub const REPLAY_ACCURACY: GeometryAccuracy = GeometryAccuracy::micrometres(10);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub repository: String,
    pub revision: String,
    pub path: String,
    pub sha256: Option<String>,
    pub extraction: String,
    pub limitations: Vec<String>,
}

/// Rings use explicit even-odd fill, board-local millimeters, Y up.
/// Extractors retain source identity and uncertainty separately for each overlay.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overlay {
    pub name: String,
    pub meaning: String,
    pub rings: Vec<Ring>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fixture {
    pub version: u32,
    pub id: String,
    pub provenance: Provenance,
    pub tolerance_mm: f64,
    pub flatten_mm: f64,
    pub substrate: Vec<Ring>,
    pub removal: Vec<Ring>,
    pub overlays: Vec<Overlay>,
    /// Source facts only; no inferred laminate or elastic constants.
    pub evidence: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Completed,
    MissingSource,
    MalformedSource,
    GeometryRejected,
    ToleranceAmbiguous,
    PhysicalFailure,
    NumericalFailure,
    SearchFailure,
    Timeout,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub fixture: String,
    pub component: String,
    pub status: Status,
    pub message: String,
    pub before_area_mm2: Option<f64>,
    pub after_area_mm2: Option<f64>,
    /// Null means unavailable, never zero or an assumed successful response.
    pub physical_metrics: Option<serde_json::Value>,
    pub after: Vec<Ring>,
}

impl Report {
    pub fn outcome(id: &str, component: &str, status: Status, message: impl Into<String>) -> Self {
        Self {
            fixture: id.into(),
            component: component.into(),
            status,
            message: message.into(),
            before_area_mm2: None,
            after_area_mm2: None,
            physical_metrics: None,
            after: vec![],
        }
    }
}

pub fn region(rings: &[Ring], tolerance: f64) -> Result<ContourSet, AccuracyError> {
    ContourSet::from_rings(
        rings.to_vec(),
        FillRule::EvenOdd,
        Resolution::new(tolerance, REPLAY_ACCURACY),
    )
}

pub fn validate(f: &Fixture) -> Result<(), String> {
    if f.version != VERSION {
        return Err(format!("unsupported schema version {}", f.version));
    }
    if f.id.is_empty()
        || !f.tolerance_mm.is_finite()
        || f.tolerance_mm <= 0.0
        || !f.flatten_mm.is_finite()
        || f.flatten_mm <= 0.0
    {
        return Err("id and finite positive tolerances required".into());
    }
    for ring in f
        .substrate
        .iter()
        .chain(&f.removal)
        .chain(f.overlays.iter().flat_map(|o| &o.rings))
    {
        if ring.len() < 3 || ring.iter().flatten().any(|v| !v.is_finite()) {
            return Err("each polygon ring requires at least three finite points".into());
        }
    }
    Ok(())
}

/// Prep components implement this small contract without needing a panelizer.
/// A solver owns its physical assumptions, termination policy and failure classification.
pub trait Component {
    fn name(&self) -> &str;
    fn evaluate(&self, fixture: &Fixture) -> Report;
}

pub struct Geometry;
impl Component for Geometry {
    fn name(&self) -> &str {
        "geometry"
    }
    fn evaluate(&self, f: &Fixture) -> Report {
        match evaluate_geometry(f) {
            Ok(report) => report,
            Err(error) => Report::outcome(
                &f.id,
                self.name(),
                Status::NumericalFailure,
                error.to_string(),
            ),
        }
    }
}

fn evaluate_geometry(f: &Fixture) -> Result<Report, AccuracyError> {
    let before = region(&f.substrate, f.tolerance_mm)?;
    let after = before.difference(&region(&f.removal, f.tolerance_mm)?)?;
    let status = if before.is_empty() || after.is_empty() {
        Status::GeometryRejected
    } else {
        Status::Completed
    };
    let mut report = Report::outcome(
        &f.id,
        "geometry",
        status,
        "Even-odd polygon regularization and explicit removal only; not attachment, topology, source-curve accuracy, or physical fracture validation.",
    );
    report.before_area_mm2 = Some(before.area());
    report.after_area_mm2 = Some(after.area());
    report.after = after.rings;
    Ok(report)
}

pub fn replay(f: &Fixture, component: &dyn Component) -> Report {
    match validate(f) {
        Ok(()) => component.evaluate(f),
        Err(message) => Report::outcome(&f.id, component.name(), Status::MalformedSource, message),
    }
}

pub fn load(path: &std::path::Path, component: &str) -> Result<Fixture, Box<Report>> {
    let id = path.file_name().unwrap_or_default().to_string_lossy();
    let bytes = std::fs::read(path).map_err(|error| {
        Box::new(Report::outcome(
            &id,
            component,
            if error.kind() == std::io::ErrorKind::NotFound {
                Status::MissingSource
            } else {
                Status::MalformedSource
            },
            error.to_string(),
        ))
    })?;
    let bytes = if path.extension().is_some_and(|ext| ext == "zst") {
        zstd::decode_all(bytes.as_slice()).map_err(|error| {
            Box::new(Report::outcome(
                &id,
                component,
                Status::MalformedSource,
                error.to_string(),
            ))
        })?
    } else {
        bytes
    };
    serde_json::from_slice(&bytes).map_err(|error| {
        Box::new(Report::outcome(
            &id,
            component,
            Status::MalformedSource,
            error.to_string(),
        ))
    })
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn svg(f: &Fixture, after: Option<&[Ring]>) -> String {
    use pcb_ir::{
        geom::{BBox, Point, region::rings_to_contours},
        render::svg_path_data,
    };
    let all = f
        .substrate
        .iter()
        .chain(&f.removal)
        .chain(f.overlays.iter().flat_map(|o| &o.rings))
        .flatten();
    let bbox = all.fold(BBox::empty(), |mut bbox, point| {
        bbox.include_point(Point::new(point[0], point[1]));
        bbox
    });
    if bbox.is_empty() {
        return "<p>No geometry available</p>".into();
    }
    let path = |rings: &[Ring], color: &str| {
        format!(
            "<path d=\"{}\" fill=\"{color}\" fill-opacity=\"0.45\" stroke=\"{color}\" stroke-width=\"0.12\" fill-rule=\"evenodd\"/>",
            svg_path_data(&rings_to_contours(rings.to_vec()))
        )
    };
    let mut body = path(after.unwrap_or(&f.substrate), "#287d50");
    body.push_str(&path(&f.removal, "#c83932"));
    // Apply opacity to the complete overlay group, not independently per layer:
    // overlapping copper layers must not cumulatively hide the substrate.
    body.push_str("<g class=\"overlays\" opacity=\"0.4\">");
    for overlay in &f.overlays {
        body.push_str(&format!(
            "<g><title>{}</title>{}</g>",
            escape(&overlay.name),
            path(&overlay.rings, "#3b66bf")
        ));
    }
    body.push_str("</g>");
    format!(
        "<svg role=\"img\" aria-label=\"{} geometry\" viewBox=\"{} {} {} {}\"><g transform=\"scale(1,-1)\">{body}</g></svg>",
        if after.is_some() { "After" } else { "Before" },
        bbox.min.x - 1.0,
        -bbox.max.y - 1.0,
        bbox.width() + 2.0,
        bbox.height() + 2.0
    )
}

/// Static self-contained report: no scripts, fonts, remote resources or source parsing.
pub fn html(rows: &[(Option<Fixture>, Report)]) -> String {
    let mut out = String::from(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Panel geometry corpus</title><style>body{font:16px system-ui;max-width:1100px;margin:2em auto;padding:0 1em;color:#222}article{border-top:1px solid #aaa;margin-top:2em}svg{width:100%;height:260px;background:#f5f5f5}.pair{display:grid;grid-template-columns:1fr 1fr;gap:1em}pre{white-space:pre-wrap;overflow-wrap:anywhere}dt{font-weight:bold}figure{margin:0}@media(max-width:600px){.pair{grid-template-columns:1fr}}</style><h1>Panel geometry corpus · v1</h1><p>Millimeters · Y up · even-odd polygons. Green: substrate; red: supplied removal; blue: labelled source overlays (not inferred obstacles).</p><p>Polygon topology and tolerance are representation choices. No claim of source-curve accuracy, support feasibility or fracture validation. Physical metrics are unavailable unless a physical component calculates them.</p>",
    );
    out.push_str("<style>.show-overlays:not(:checked)~.pair .overlays{display:none}</style>");
    for (index, (fixture, report)) in rows.iter().enumerate() {
        out.push_str(&format!(
            "<article><h2>{}</h2><p><strong>{} · {}</strong></p><p>{}</p>",
            escape(&report.fixture),
            escape(&report.component),
            serde_json::to_value(&report.status)
                .unwrap()
                .as_str()
                .unwrap(),
            escape(&report.message)
        ));
        if let Some(f) = fixture.as_ref().filter(|f| validate(f).is_ok()) {
            out.push_str(&format!("<input class=\"show-overlays\" id=\"overlays-{index}\" type=\"checkbox\"><label for=\"overlays-{index}\">Show source overlays</label>"));
            out.push_str(&format!("<p>Region significance tolerance: {} mm; extraction flatten tolerance: {} mm. Input rings: {}; after rings: {} (includes holes).</p><div class=\"pair\"><figure><figcaption>Before</figcaption>{}</figure><figure><figcaption>After</figcaption>{}</figure></div>", f.tolerance_mm, f.flatten_mm, f.substrate.len(), report.after.len(), svg(f, None), if report.after_area_mm2.is_some() { svg(f, Some(&report.after)) } else { "<p>After geometry unavailable</p>".into() }));
            out.push_str("<details><summary>Overlay identities and semantics</summary><ul>");
            for o in &f.overlays {
                out.push_str(&format!(
                    "<li>{}: {}</li>",
                    escape(&o.name),
                    escape(&o.meaning)
                ));
            }
            out.push_str(&format!("</ul></details><details><summary>Source provenance and evidence</summary><pre>{}</pre><pre>{}</pre></details>", escape(&serde_json::to_string_pretty(&f.provenance).unwrap()), escape(&serde_json::to_string_pretty(&f.evidence).unwrap())));
        }
        out.push_str(&format!(
            "<p>Area before: {} mm²; after: {} mm².</p><p>Physical metrics: {}</p></article>",
            metric(report.before_area_mm2),
            metric(report.after_area_mm2),
            report
                .physical_metrics
                .as_ref()
                .map(|m| escape(&m.to_string()))
                .unwrap_or(
                    "unavailable — no material, loads, constraints or mechanical model supplied"
                        .into()
                )
        ));
    }
    out.push_str("</html>");
    out
}

fn metric(value: Option<f64>) -> String {
    value
        .map(|v| format!("{v:.6}"))
        .unwrap_or("unavailable".into())
}
