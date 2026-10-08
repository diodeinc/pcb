//! The DFM report as a single SQLite database. Every distinct layer, subject
//! and evidence shape is stored once and referenced by id; coordinates are
//! integer nanometres. A finding's or site's short lists are JSON arrays in
//! its own row, readable with SQLite's JSON functions:
//!
//! - `layers`, `subjects`: ids into those tables
//! - `witnesses`: `[role, x, y]`
//! - `evidence`: `[role, shape id]`

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, Statement, params};
use serde::Serialize;
use serde_json::json;

use super::report::{
    DfmReport, Evidence, EvidenceDisplay, LayerRef, ReportBBox, ReportPoint, Subject, Witness,
    to_nanometre,
};

const SCHEMA: &str = "
CREATE TABLE report (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID;
CREATE TABLE rules (
    id INTEGER PRIMARY KEY, rule_id TEXT NOT NULL UNIQUE, severity TEXT NOT NULL,
    status TEXT NOT NULL, finding_count INTEGER NOT NULL, waived_count INTEGER NOT NULL,
    detail TEXT NOT NULL
);
CREATE TABLE layers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, function TEXT NOT NULL, side TEXT);
CREATE TABLE subjects (
    id INTEGER PRIMARY KEY, role TEXT NOT NULL, kind TEXT NOT NULL, name TEXT,
    reference_designator TEXT, pin TEXT, net TEXT, padstack_ref TEXT,
    source TEXT, provenance TEXT, drill_span TEXT
);
-- `paths` and `display_paths` hold little-endian i32: the path count, each
-- path's point count, then every point's x and y as a delta from the last.
CREATE TABLE shapes (
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL,
    center_x INTEGER, center_y INTEGER, diameter REAL,
    start_x INTEGER, start_y INTEGER, end_x INTEGER, end_y INTEGER,
    min_x INTEGER, min_y INTEGER, max_x INTEGER, max_y INTEGER,
    paths BLOB, display TEXT, display_paths BLOB
);
CREATE TABLE findings (
    id INTEGER PRIMARY KEY, finding_id TEXT NOT NULL UNIQUE,
    rule INTEGER NOT NULL REFERENCES rules, severity TEXT NOT NULL,
    waived INTEGER NOT NULL, waiver_reason TEXT, title TEXT NOT NULL, message TEXT NOT NULL,
    measurement TEXT NOT NULL, frame INTEGER NOT NULL, x INTEGER, y INTEGER,
    min_x INTEGER, min_y INTEGER, max_x INTEGER, max_y INTEGER,
    layers TEXT NOT NULL, subjects TEXT NOT NULL, witnesses TEXT NOT NULL, evidence TEXT NOT NULL
);
CREATE TABLE sites (
    finding INTEGER NOT NULL REFERENCES findings, position INTEGER NOT NULL,
    site_id TEXT NOT NULL, measurement TEXT NOT NULL, measurement_kind TEXT NOT NULL,
    uncertainty_mm REAL NOT NULL,
    min_x INTEGER NOT NULL, min_y INTEGER NOT NULL, max_x INTEGER NOT NULL, max_y INTEGER NOT NULL,
    note TEXT,
    layers TEXT NOT NULL, subjects TEXT NOT NULL, witnesses TEXT NOT NULL, evidence TEXT NOT NULL,
    PRIMARY KEY (finding, position)
) WITHOUT ROWID;
CREATE TABLE scene (id INTEGER PRIMARY KEY, label TEXT NOT NULL, feature TEXT NOT NULL, layer TEXT, color TEXT NOT NULL, svg TEXT NOT NULL);
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

fn write(fill: impl FnOnce(&Connection) -> Result<()>) -> Result<Vec<u8>> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(SCHEMA)?;
    let transaction = connection.unchecked_transaction()?;
    fill(&transaction)?;
    transaction.commit()?;
    connection.execute_batch("VACUUM")?;
    Ok(connection.serialize("main")?.to_vec())
}

struct Writer<'a> {
    report: Statement<'a>,
    rule: Statement<'a>,
    layer: Statement<'a>,
    subject: Statement<'a>,
    shape: Statement<'a>,
    finding: Statement<'a>,
    site: Statement<'a>,
    scene: Statement<'a>,
    layers: HashMap<Vec<u8>, i64>,
    subjects: HashMap<Vec<u8>, i64>,
    shapes: HashMap<Vec<u8>, i64>,
}

/// A finding's or site's lists, each a JSON array.
struct Members {
    layers: String,
    subjects: String,
    witnesses: String,
    evidence: String,
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
            rule: insert("rules", 7)?,
            layer: insert("layers", 4)?,
            subject: insert("subjects", 11)?,
            shape: insert("shapes", 16)?,
            finding: insert("findings", 20)?,
            site: insert("sites", 15)?,
            scene: insert("scene", 6)?,
            layers: HashMap::new(),
            subjects: HashMap::new(),
            shapes: HashMap::new(),
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
            ("waivers", text(&report.waivers)),
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
        let rules = report
            .rules
            .iter()
            .zip(0_i64..)
            .map(|(rule, id)| {
                self.rule.execute(params![
                    id,
                    rule.id,
                    label(&rule.severity),
                    label(&rule.status),
                    rule.finding_count as i64,
                    rule.waived_count as i64,
                    text(rule),
                ])?;
                Ok((rule.id.as_str(), id))
            })
            .collect::<Result<HashMap<_, _>>>()?;
        for (finding, id) in report.findings.iter().zip(0_i64..) {
            let point = finding.location.point.map(nanometres);
            let bbox = finding.location.bounding_box.map(bounds);
            let members = self.members(
                report,
                &finding.location.witnesses,
                &finding.layers,
                &finding.subjects,
                &finding.evidence,
            )?;
            self.finding.execute(params![
                id,
                finding.id,
                rules[finding.rule_id.as_str()],
                label(&finding.severity),
                finding.waived,
                finding.waiver_reason,
                finding.title,
                finding.message,
                text(&finding.measurement),
                finding.frame,
                point.map(|[x, _]| x),
                point.map(|[_, y]| y),
                bbox.map(|bbox| bbox[0]),
                bbox.map(|bbox| bbox[1]),
                bbox.map(|bbox| bbox[2]),
                bbox.map(|bbox| bbox[3]),
                members.layers,
                members.subjects,
                members.witnesses,
                members.evidence,
            ])?;
            for (site, position) in finding.sites.iter().zip(0_i64..) {
                let [min_x, min_y, max_x, max_y] = bounds(site.bounding_box);
                let members = self.members(
                    report,
                    &site.witnesses,
                    &site.layers,
                    &site.subjects,
                    &site.evidence,
                )?;
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
                    members.layers,
                    members.subjects,
                    members.witnesses,
                    members.evidence,
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

    fn members(
        &mut self,
        report: &DfmReport,
        witnesses: &[Witness],
        layers: &[LayerRef],
        subjects: &[Subject],
        evidence: &[Evidence],
    ) -> Result<Members> {
        let layers = layers
            .iter()
            .map(|layer| self.layer(layer))
            .collect::<Result<Vec<_>>>()?;
        let subjects = subjects
            .iter()
            .map(|subject| self.subject(subject))
            .collect::<Result<Vec<_>>>()?;
        let witnesses = witnesses
            .iter()
            .map(|witness| {
                let [x, y] = nanometres(witness.point);
                json!([witness.role, x, y])
            })
            .collect::<Vec<_>>();
        let evidence = evidence
            .iter()
            .map(|item| {
                let shape = item
                    .shared
                    .map_or(item, |index| &report.shared_evidence[index as usize]);
                Ok(json!([item.role, self.shape(shape)?]))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Members {
            layers: text(&layers),
            subjects: text(&subjects),
            witnesses: text(&witnesses),
            evidence: text(&evidence),
        })
    }

    fn layer(&mut self, layer: &LayerRef) -> Result<i64> {
        intern(&mut self.layers, layer, |id| {
            self.layer
                .execute(params![id, layer.name, layer.function, layer.side])
        })
    }

    fn subject(&mut self, subject: &Subject) -> Result<i64> {
        intern(&mut self.subjects, subject, |id| {
            self.subject.execute(params![
                id,
                subject.role,
                subject.kind,
                subject.name,
                subject.reference_designator,
                subject.pin,
                subject.net,
                subject.padstack_ref,
                subject.source.as_ref().map(text),
                subject.provenance.as_ref().map(text),
                subject.drill_span.as_ref().map(text),
            ])
        })
    }

    /// Evidence without its role, which the referring row keeps.
    fn shape(&mut self, evidence: &Evidence) -> Result<i64> {
        let center = evidence.center.map(nanometres);
        let start = evidence.start.map(nanometres);
        let end = evidence.end.map(nanometres);
        let bbox = evidence.bounding_box.map(bounds);
        let (display, display_paths) = match &evidence.display {
            Some(EvidenceDisplay::RoundStroke { paths, width_mm }) => (
                Some(text(
                    &json!({ "kind": "round_stroke", "width_mm": width_mm }),
                )),
                Some(encode(paths)),
            ),
            display => (display.as_ref().map(text), None),
        };
        let paths = (!evidence.paths.is_empty()).then(|| encode(&evidence.paths));
        let key = (
            evidence.kind,
            center,
            evidence.diameter,
            start,
            end,
            bbox,
            &paths,
            &display,
            &display_paths,
        );
        intern(&mut self.shapes, &key, |id| {
            self.shape.execute(params![
                id,
                evidence.kind,
                center.map(|[x, _]| x),
                center.map(|[_, y]| y),
                evidence.diameter,
                start.map(|[x, _]| x),
                start.map(|[_, y]| y),
                end.map(|[x, _]| x),
                end.map(|[_, y]| y),
                bbox.map(|bbox| bbox[0]),
                bbox.map(|bbox| bbox[1]),
                bbox.map(|bbox| bbox[2]),
                bbox.map(|bbox| bbox[3]),
                paths,
                display,
                display_paths,
            ])
        })
    }
}

/// The row id of `value`, inserting it under the next id the first time.
fn intern(
    ids: &mut HashMap<Vec<u8>, i64>,
    value: &impl Serialize,
    insert: impl FnOnce(i64) -> rusqlite::Result<usize>,
) -> Result<i64> {
    let key = serde_json::to_vec(value)?;
    if let Some(&id) = ids.get(&key) {
        return Ok(id);
    }
    let id = ids.len() as i64;
    insert(id)?;
    ids.insert(key, id);
    Ok(id)
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

fn encode(paths: &[Vec<ReportPoint>]) -> Vec<u8> {
    let mut last = [0, 0];
    std::iter::once(paths.len() as i64)
        .chain(paths.iter().map(|path| path.len() as i64))
        .chain(paths.iter().flatten().flat_map(|&point| {
            let point = nanometres(point);
            let delta = [point[0] - last[0], point[1] - last[1]];
            last = point;
            delta
        }))
        .flat_map(|value| (value as i32).to_le_bytes())
        .collect()
}
