//! The DFM report as a single SQLite database. Every distinct layer, subject
//! and evidence shape is stored once and referenced by id; coordinates are
//! integer nanometres. A finding's or site's short lists are JSON arrays in
//! its own row, readable with SQLite's JSON functions:
//!
//! - `layers`, `subjects`: ids into those tables
//! - `witnesses`: `[role, x, y]`
//! - `evidence`: `[role, shape id]`
//!
//! Findings say what is wrong and where; their sites hold the geometry.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;

use anyhow::{Context, Result};
use rusqlite::{Connection, Statement, params};
use serde::Serialize;
use serde_json::json;

use super::report::{
    DfmReport, Evidence, LayerRef, REPORT_SCHEMA_VERSION, ReportBBox, ReportPoint, Subject,
    Witness, to_nanometre,
};

/// `DFMR`; `user_version` is the report schema version.
const APPLICATION_ID: i32 = 0x4446_4d52;

const SCHEMA: &str = "
CREATE TABLE report (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE rules (
    id INTEGER PRIMARY KEY, rule_id TEXT NOT NULL UNIQUE, title TEXT NOT NULL,
    finding_title TEXT NOT NULL, severity TEXT NOT NULL, tier TEXT NOT NULL,
    status TEXT NOT NULL, comparison TEXT NOT NULL, limit_value REAL NOT NULL,
    limit_unit TEXT NOT NULL, limit_pdk_value TEXT NOT NULL, subject TEXT NOT NULL,
    quantity TEXT NOT NULL, method TEXT NOT NULL, checked INTEGER NOT NULL,
    finding_count INTEGER NOT NULL, skip_reason TEXT, assumptions TEXT NOT NULL,
    view TEXT NOT NULL
);
-- Measurements below a limit by less than their own uncertainty.
CREATE TABLE unresolved (
    rule INTEGER NOT NULL REFERENCES rules, frame INTEGER NOT NULL,
    actual_mm REAL NOT NULL, uncertainty_mm REAL NOT NULL,
    x INTEGER NOT NULL, y INTEGER NOT NULL, layers TEXT NOT NULL
);
CREATE TABLE layers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, function TEXT NOT NULL, side TEXT);
CREATE TABLE subjects (
    id INTEGER PRIMARY KEY, role TEXT NOT NULL, kind TEXT NOT NULL, name TEXT,
    reference_designator TEXT, pin TEXT, net TEXT, padstack_ref TEXT,
    locator TEXT, drill_span TEXT
);
-- `kind` is circle, segment, bounds, path (open), region (closed rings,
-- filled nonzero) or stroke (round-capped paths of `width_mm`). `paths` holds
-- little-endian i32: the path count, each path's point count, then every
-- point's x and y as a delta from the last.
CREATE TABLE shapes (
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL,
    center_x INTEGER, center_y INTEGER, diameter REAL,
    start_x INTEGER, start_y INTEGER, end_x INTEGER, end_y INTEGER,
    min_x INTEGER, min_y INTEGER, max_x INTEGER, max_y INTEGER,
    paths BLOB, width_mm REAL
);
CREATE TABLE findings (
    id INTEGER PRIMARY KEY, finding_id TEXT NOT NULL UNIQUE,
    rule INTEGER NOT NULL REFERENCES rules, frame INTEGER NOT NULL,
    measurement TEXT NOT NULL, message TEXT NOT NULL, x INTEGER, y INTEGER,
    min_x INTEGER, min_y INTEGER, max_x INTEGER, max_y INTEGER,
    layers TEXT NOT NULL, subjects TEXT NOT NULL
);
CREATE INDEX findings_rule ON findings (rule);
CREATE TABLE sites (
    finding INTEGER NOT NULL REFERENCES findings, position INTEGER NOT NULL,
    site_id TEXT NOT NULL, measurement TEXT NOT NULL, measurement_kind TEXT NOT NULL,
    uncertainty_mm REAL NOT NULL,
    min_x INTEGER NOT NULL, min_y INTEGER NOT NULL, max_x INTEGER NOT NULL, max_y INTEGER NOT NULL,
    note TEXT,
    layers TEXT NOT NULL, subjects TEXT NOT NULL, witnesses TEXT NOT NULL, evidence TEXT NOT NULL,
    PRIMARY KEY (finding, position)
) WITHOUT ROWID;
CREATE TABLE scene (
    id INTEGER PRIMARY KEY, label TEXT NOT NULL, feature TEXT NOT NULL, layer TEXT,
    color TEXT NOT NULL, svg TEXT NOT NULL
);
CREATE VIEW finding_summary AS
SELECT
    f.id, f.finding_id, r.rule_id, r.severity, r.finding_title AS title, f.message,
    coalesce(f.measurement ->> '$.actual_mm', f.measurement ->> '$.actual_count',
        f.measurement ->> '$.actual_ratio') AS actual,
    r.limit_value AS limit_value, r.limit_unit AS unit,
    coalesce(f.measurement ->> '$.margin_mm', f.measurement ->> '$.margin_count',
        f.measurement ->> '$.margin_ratio') AS margin,
    (SELECT group_concat(l.name, ', ') FROM json_each(f.layers) j JOIN layers l ON l.id = j.value)
        AS layers,
    (SELECT group_concat(DISTINCT s.net) FROM json_each(f.subjects) j JOIN subjects s ON s.id = j.value)
        AS nets,
    (SELECT count(*) FROM sites s WHERE s.finding = f.id) AS sites,
    f.frame, f.x / 1e6 AS x_mm, f.y / 1e6 AS y_mm
FROM findings f JOIN rules r ON r.id = f.rule;
";

/// The report's database file, byte for byte the same for the same report.
pub(super) fn database(report: &DfmReport) -> Result<Vec<u8>> {
    write(|connection| Writer::new(connection)?.report(report))
}

/// A report that could not be built: its metadata alone.
pub(super) fn incomplete_database(report: &serde_json::Value) -> Result<Vec<u8>> {
    write(|connection| {
        let mut insert = connection.prepare("INSERT INTO report VALUES (?, ?)")?;
        for (key, value) in report.as_object().into_iter().flatten() {
            insert.execute(params![key, text(value)])?;
        }
        Ok(())
    })
}

/// Fill a fresh in-memory database in one transaction. The same inserts in
/// the same order lay out the same pages, so the file is reproducible.
fn write(fill: impl FnOnce(&Connection) -> Result<()>) -> Result<Vec<u8>> {
    let connection = Connection::open_in_memory()?;
    connection.pragma_update(None, "application_id", APPLICATION_ID)?;
    connection.pragma_update(None, "user_version", REPORT_SCHEMA_VERSION)?;
    connection.execute_batch(SCHEMA)?;
    let transaction = connection.unchecked_transaction()?;
    fill(&transaction)?;
    transaction.commit()?;
    Ok(connection.serialize("main")?.to_vec())
}

/// A shape row: evidence without the role its referrers keep.
#[derive(PartialEq, Eq, Hash)]
struct Shape {
    kind: &'static str,
    center: Option<[i64; 2]>,
    diameter: Option<u64>,
    start: Option<[i64; 2]>,
    end: Option<[i64; 2]>,
    bounds: Option<[i64; 4]>,
    paths: Option<Vec<u8>>,
    width_mm: Option<u64>,
}

impl Shape {
    fn new(evidence: &Evidence) -> Result<Self> {
        Ok(Self {
            kind: evidence.kind,
            center: evidence.center.map(nanometres),
            diameter: evidence.diameter.map(f64::to_bits),
            start: evidence.start.map(nanometres),
            end: evidence.end.map(nanometres),
            bounds: evidence.bounding_box.map(bounds),
            paths: (!evidence.paths.is_empty())
                .then(|| encode(&evidence.paths))
                .transpose()?,
            width_mm: evidence.width_mm.map(f64::to_bits),
        })
    }
}

/// The ids of rows stored once, by content.
struct Interned<K> {
    ids: HashMap<K, i64>,
}

impl<K: Eq + Hash> Interned<K> {
    fn new() -> Self {
        Self {
            ids: HashMap::new(),
        }
    }

    /// The id of `key`'s row, inserted under the next id the first time.
    fn id(
        &mut self,
        key: K,
        insert: impl FnOnce(&K, i64) -> rusqlite::Result<usize>,
    ) -> Result<i64> {
        let next = self.ids.len() as i64;
        Ok(match self.ids.entry(key) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(entry) => {
                insert(entry.key(), next)?;
                *entry.insert(next)
            }
        })
    }
}

struct Writer<'a> {
    report: Statement<'a>,
    rule: Statement<'a>,
    unresolved: Statement<'a>,
    layer: Statement<'a>,
    subject: Statement<'a>,
    shape: Statement<'a>,
    finding: Statement<'a>,
    site: Statement<'a>,
    scene: Statement<'a>,
    layers: Interned<String>,
    subjects: Interned<String>,
    shapes: Interned<Shape>,
}

impl<'a> Writer<'a> {
    fn new(connection: &'a Connection) -> Result<Self> {
        let insert = |table: &str, columns: usize| {
            connection.prepare(&format!(
                "INSERT INTO {table} VALUES ({})",
                vec!["?"; columns].join(", ")
            ))
        };
        Ok(Self {
            report: insert("report", 2)?,
            rule: insert("rules", 19)?,
            unresolved: insert("unresolved", 7)?,
            layer: insert("layers", 4)?,
            subject: insert("subjects", 10)?,
            shape: insert("shapes", 15)?,
            finding: insert("findings", 14)?,
            site: insert("sites", 15)?,
            scene: insert("scene", 6)?,
            layers: Interned::new(),
            subjects: Interned::new(),
            shapes: Interned::new(),
        })
    }

    fn report(&mut self, report: &DfmReport) -> Result<()> {
        let metadata = [
            ("schema_version", text(&report.schema_version)),
            ("generated_at", text(&report.generated_at)),
            ("verdict", text(&report.verdict)),
            ("tool", text(&report.tool)),
            ("input", text(&report.input)),
            ("pdk", text(&report.pdk)),
            ("layout_target", text(&report.layout_target)),
            ("coordinate_system", text(&report.coordinate_system)),
            ("layout", text(&report.layout)),
            ("summary", text(&report.summary)),
            ("frames", text(&report.frames)),
            (
                "scene",
                text(&json!({
                    "schema_version": report.scene.schema_version,
                    "bounds": report.scene.bounds,
                })),
            ),
        ];
        for (key, value) in metadata {
            self.report.execute(params![key, value])?;
        }
        let mut rules = HashMap::new();
        for (rule, id) in report.rules.iter().zip(0_i64..) {
            self.rule.execute(params![
                id,
                rule.id,
                rule.title,
                rule.finding_title,
                label(&rule.severity),
                rule.tier,
                label(&rule.status),
                rule.comparison,
                rule.limit.normalized_value,
                rule.limit.normalized_unit,
                rule.limit.pdk_value,
                rule.subject,
                rule.quantity,
                rule.method,
                rule.checked as i64,
                rule.finding_count as i64,
                rule.skip_reason,
                text(&rule.assumptions),
                text(&rule.view),
            ])?;
            for unresolved in &rule.unresolved {
                let [x, y] = nanometres(unresolved.point);
                self.unresolved.execute(params![
                    id,
                    unresolved.frame,
                    to_nanometre(unresolved.actual_mm),
                    to_nanometre(unresolved.uncertainty_mm),
                    x,
                    y,
                    text(&unresolved.layers),
                ])?;
            }
            rules.insert(rule.id.as_str(), id);
        }
        for (finding, id) in report.findings.iter().zip(0_i64..) {
            let rule = rules
                .get(finding.rule_id.as_str())
                .with_context(|| format!("finding {} names no rule", finding.id))?;
            let point = finding.location.point.map(nanometres);
            let bbox = finding.location.bounding_box.map(bounds);
            let layers = self.layers(&finding.layers)?;
            let subjects = self.subjects(&finding.subjects)?;
            self.finding.execute(params![
                id,
                finding.id,
                rule,
                finding.frame,
                text(&finding.measurement),
                finding.message,
                point.map(|[x, _]| x),
                point.map(|[_, y]| y),
                bbox.map(|bbox| bbox[0]),
                bbox.map(|bbox| bbox[1]),
                bbox.map(|bbox| bbox[2]),
                bbox.map(|bbox| bbox[3]),
                layers,
                subjects,
            ])?;
            for (site, position) in finding.sites.iter().zip(0_i64..) {
                let [min_x, min_y, max_x, max_y] = bounds(site.bounding_box);
                let layers = self.layers(&site.layers)?;
                let subjects = self.subjects(&site.subjects)?;
                let evidence = site
                    .evidence
                    .iter()
                    .map(|item| Ok((item.role, self.shape(item)?)))
                    .collect::<Result<Vec<_>>>()?;
                self.site.execute(params![
                    id,
                    position,
                    site.id,
                    text(&site.measurement),
                    label(&site.measurement_kind),
                    to_nanometre(site.uncertainty_mm),
                    min_x,
                    min_y,
                    max_x,
                    max_y,
                    site.note,
                    layers,
                    subjects,
                    witnesses(&site.witnesses),
                    text(&evidence),
                ])?;
            }
        }
        for (pass, id) in report.scene.passes.iter().zip(0_i64..) {
            self.scene.execute(params![
                id,
                pass.label,
                pass.feature,
                pass.layer,
                pass.color,
                pass.svg
            ])?;
        }
        Ok(())
    }

    /// The ids of `layers`, as a JSON array.
    fn layers(&mut self, layers: &[LayerRef]) -> Result<String> {
        let ids = layers
            .iter()
            .map(|layer| {
                self.layers.id(text(layer), |_, id| {
                    self.layer
                        .execute(params![id, layer.name, layer.function, layer.side])
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(text(&ids))
    }

    /// The ids of `subjects`, as a JSON array.
    fn subjects(&mut self, subjects: &[Subject]) -> Result<String> {
        let ids = subjects
            .iter()
            .map(|subject| {
                self.subjects.id(text(subject), |_, id| {
                    self.subject.execute(params![
                        id,
                        subject.role,
                        subject.kind,
                        subject.name,
                        subject.reference_designator,
                        subject.pin,
                        subject.net,
                        subject.padstack_ref,
                        subject
                            .provenance
                            .as_ref()
                            .or(subject.source.as_ref())
                            .map(text),
                        subject.drill_span.as_ref().map(text),
                    ])
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(text(&ids))
    }

    fn shape(&mut self, evidence: &Evidence) -> Result<i64> {
        self.shapes.id(Shape::new(evidence)?, |shape, id| {
            let x = |point: Option<[i64; 2]>| point.map(|[x, _]| x);
            let y = |point: Option<[i64; 2]>| point.map(|[_, y]| y);
            self.shape.execute(params![
                id,
                shape.kind,
                x(shape.center),
                y(shape.center),
                shape.diameter.map(f64::from_bits),
                x(shape.start),
                y(shape.start),
                x(shape.end),
                y(shape.end),
                shape.bounds.map(|bounds| bounds[0]),
                shape.bounds.map(|bounds| bounds[1]),
                shape.bounds.map(|bounds| bounds[2]),
                shape.bounds.map(|bounds| bounds[3]),
                shape.paths,
                shape.width_mm.map(f64::from_bits),
            ])
        })
    }
}

fn witnesses(witnesses: &[Witness]) -> String {
    let witnesses = witnesses
        .iter()
        .map(|witness| {
            let [x, y] = nanometres(witness.point);
            (witness.role, x, y)
        })
        .collect::<Vec<_>>();
    text(&witnesses)
}

fn text(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("report records serialize")
}

/// A unit enum as its bare snake_case name.
fn label(value: &impl Serialize) -> String {
    text(value).trim_matches('"').to_owned()
}

fn nanometres(point: ReportPoint) -> [i64; 2] {
    [point.x, point.y].map(|millimetres| (millimetres * 1e6).round() as i64)
}

fn bounds(bbox: ReportBBox) -> [i64; 4] {
    let [min_x, min_y] = nanometres(bbox.min);
    let [max_x, max_y] = nanometres(bbox.max);
    [min_x, min_y, max_x, max_y]
}

fn encode(paths: &[Vec<ReportPoint>]) -> Result<Vec<u8>> {
    let mut last = [0, 0];
    let deltas = paths.iter().flatten().flat_map(|&point| {
        let point = nanometres(point);
        let delta = [point[0] - last[0], point[1] - last[1]];
        last = point;
        delta
    });
    std::iter::once(paths.len() as i64)
        .chain(paths.iter().map(|path| path.len() as i64))
        .chain(deltas)
        .try_fold(Vec::new(), |mut bytes, value| {
            let value = i32::try_from(value).context("evidence geometry spans more than 2.1 m")?;
            bytes.extend(value.to_le_bytes());
            Ok(bytes)
        })
}
