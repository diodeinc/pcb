//! Maximum plated-hole aspect ratio.
//!
//! For each circular plated hole `h`, the finished drilled diameter `dₕ` is
//! stated by IPC-2581 and the physical stackup supplies the thickness `tₕ`
//! traversed by its drill span. The scalar measurement is
//!
//! ```text
//! aₕ = tₕ / dₕ ≤ A_max.
//! ```
//!
//! Through holes use the finished total stackup thickness. A resolved blind
//! or buried span sums only physical stackup layers between its two copper
//! endpoints, inclusive. The selected profile's board-thickness default may
//! replace incomplete IPC thickness only for a declared through hole.

use crate::commands::dfm::design::{Design, HoleClass, SpanThickness, ThicknessSource};
use crate::commands::dfm::report::Evidence;
use crate::commands::dfm::rules::Conditions;

use super::{RatioEvaluation, RatioMeasured, hole_subject, holes_of_class};

pub(super) fn evaluate(
    class: HoleClass,
    conditions: &Conditions,
    design: &Design,
) -> RatioEvaluation {
    let holes = holes_of_class(design, class);
    let stackup = design
        .stackup
        .as_ref()
        .expect("hole aspect-ratio rules request physical stackup extraction");
    let mut measured = Vec::with_capacity(holes.len());
    let mut assumptions = Vec::new();

    for (_, hole) in &holes {
        let thickness = match stackup.span_thickness(&hole.drill_span) {
            Ok(thickness) => thickness,
            Err(reason) if hole.drill_span.interpretation == "declared_through_board" => {
                let Some(default) = conditions.assumed_board_thickness.as_ref() else {
                    return incomplete(holes.len(), hole, class, reason, assumptions);
                };
                let assumption = format!(
                    "IPC-2581 through-hole thickness is incomplete ({reason}); used selected profile defaults.board_thickness = '{}'.",
                    default.original()
                );
                if assumptions.is_empty() {
                    assumptions.push(assumption.clone());
                }
                SpanThickness {
                    millimeters: default.millimeters(),
                    source: ThicknessSource::ProfileDefaultBoardThickness,
                }
            }
            Err(reason) => return incomplete(holes.len(), hole, class, reason, assumptions),
        };
        let actual_ratio = thickness.millimeters / hole.diameter_mm;
        if !(actual_ratio.is_finite() && actual_ratio > 0.0) {
            return incomplete(
                holes.len(),
                hole,
                class,
                "computed ratio is not positive and finite".to_owned(),
                assumptions,
            );
        }
        let thickness_source = thickness.source.label();
        let subject = hole_subject(design, hole, "offender");
        let evidence = vec![Evidence::circle(
            "drilled_hole",
            hole.center,
            hole.diameter_mm,
        )];
        measured.push(RatioMeasured {
            actual_ratio,
            drilled_span_thickness_mm: thickness.millimeters,
            finished_hole_diameter_mm: hole.diameter_mm,
            thickness_source,
            center: hole.center,
            bbox: hole.bbox,
            layers: vec![hole.layer.clone()],
            subjects: vec![subject],
            evidence,
            note: format!(
                "Aspect ratio uses {:.6} mm drilled-span thickness from {thickness_source} and {:.6} mm finished circular hole diameter.",
                thickness.millimeters, hole.diameter_mm
            ),
        });
    }

    RatioEvaluation {
        checked: holes.len(),
        measured,
        incomplete_reason: None,
        assumptions,
    }
}

fn incomplete(
    checked: usize,
    hole: &crate::commands::dfm::design::Hole,
    class: HoleClass,
    reason: String,
    assumptions: Vec<String>,
) -> RatioEvaluation {
    RatioEvaluation {
        checked,
        measured: Vec::new(),
        incomplete_reason: Some(format!(
            "incomplete physical thickness data for {} hole at ({:.6}, {:.6}): {reason}",
            class.label(),
            hole.center.x,
            hole.center.y
        )),
        assumptions,
    }
}

#[cfg(test)]
mod tests {
    use crate::LayoutTarget;
    use crate::commands::dfm::{
        CheckRequest, PdkSource, TextSource, check,
        report::{DfmReport, Measurement, RuleResult, RuleStatus, Verdict},
    };
    use crate::ipc2581::Ipc2581;
    use pcb_ir::import::ipc2581::import_design;

    const BOARD: &str = r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
    <LayerRef name="TOP"/><LayerRef name="INNER1"/><LayerRef name="INNER2"/><LayerRef name="BOTTOM"/>
    <LayerRef name="DRILL"/><LayerRef name="BLIND"/><LayerRef name="BURIED"/>
  </Content>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
    <Layer name="INNER2" layerFunction="SIGNAL" side="INTERNAL" polarity="POSITIVE"/>
    <Layer name="INNER1" layerFunction="SIGNAL" side="INTERNAL" polarity="POSITIVE"/>
    <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
    <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE"><Span fromLayer="TOP" toLayer="BOTTOM"/></Layer>
    <Layer name="BLIND" layerFunction="DRILL" side="ALL" polarity="POSITIVE"><Span fromLayer="TOP" toLayer="INNER1"/></Layer>
    <Layer name="BURIED" layerFunction="DRILL" side="ALL" polarity="POSITIVE"><Span fromLayer="INNER1" toLayer="INNER2"/></Layer>
    <Stackup name="Primary" overallThickness="1.6" whereMeasured="METAL">
      <StackupGroup name="Primary_Group" thickness="1.6">
        <StackupLayer layerOrGroupRef="TOP" thickness="0.05" sequence="0"/>
        <StackupLayer layerOrGroupRef="D1" thickness="0.20" sequence="1"/>
        <StackupLayer layerOrGroupRef="INNER1" thickness="0.05" sequence="2"/>
        <StackupLayer layerOrGroupRef="D2" thickness="0.40" sequence="3"/>
        <StackupLayer layerOrGroupRef="INNER2" thickness="0.05" sequence="4"/>
        <StackupLayer layerOrGroupRef="D3" thickness="0.80" sequence="5"/>
        <StackupLayer layerOrGroupRef="BOTTOM" thickness="0.05" sequence="6"/>
      </StackupGroup>
    </Stackup>
    <Step name="board" type="BOARD">
      <LayerFeature layerRef="DRILL"><Set><Hole name="PTH" diameter="0.2" platingStatus="PLATED" x="1" y="1"/></Set></LayerFeature>
      <LayerFeature layerRef="BLIND"><Set><Hole name="BLIND" diameter="0.03" platingStatus="VIA" x="2" y="2"/></Set></LayerFeature>
      <LayerFeature layerRef="BURIED"><Set><Hole name="BURIED" diameter="0.05" platingStatus="VIA" x="3" y="3"/></Set></LayerFeature>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#;

    const PTH_PDK: &str = r#"schema_version = 2
default_profile = "test"
[pdk]
id = "test"
name = "Test"
revision = "1"
[profiles.test]
name = "Test"
[[rules.drilling.hole_aspect_ratio]]
id = "pth-aspect-ratio"
select = { hole = "pth" }
limit = { maximum = 8.0 }
"#;

    const PTH_PDK_WITH_DEFAULT: &str = r#"schema_version = 2
default_profile = "test"
[pdk]
id = "test"
name = "Test"
revision = "1"
[profiles.test]
name = "Test"
[profiles.test.defaults]
board_thickness = "1.6 mm"
[[rules.drilling.hole_aspect_ratio]]
id = "pth-aspect-ratio"
select = { hole = "pth" }
limit = { maximum = 8.0 }
"#;

    const VIA_PDK_WITH_DEFAULT: &str = r#"schema_version = 2
default_profile = "test"
[pdk]
id = "test"
name = "Test"
revision = "1"
[profiles.test]
name = "Test"
[profiles.test.defaults]
board_thickness = "1.6 mm"
[[rules.drilling.hole_aspect_ratio]]
id = "via-aspect-ratio"
select = { hole = "via" }
cases = [
  { id = "4-layer", when = { copper_layers = { exact = 4 } }, limit = { maximum = 8.0 } },
]
"#;

    fn run(xml: &str, pdk: &str) -> DfmReport {
        let ipc = Ipc2581::parse(xml).unwrap();
        let imported = import_design(&ipc).unwrap();
        check(
            &imported,
            CheckRequest {
                input: crate::commands::dfm::report::FileIdentity::new("board.xml", xml.as_bytes()),
                pdk: PdkSource::Toml(TextSource {
                    path: "test.toml",
                    source: pdk,
                }),
                waivers: None,
                layout_target: LayoutTarget::Board,
                generated_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            },
        )
        .unwrap()
    }

    fn only_rule(report: &DfmReport) -> &RuleResult {
        assert_eq!(report.rules.len(), 1);
        &report.rules[0]
    }

    #[test]
    fn through_hole_passes_at_boundary_and_prefers_explicit_overall_thickness() {
        let pdk = PTH_PDK_WITH_DEFAULT.replace("1.6 mm", "3.2 mm");
        let report = run(BOARD, &pdk);
        let rule = only_rule(&report);
        assert!(matches!(report.verdict, Verdict::Pass));
        assert!(matches!(rule.status, RuleStatus::Pass));
        assert_eq!(rule.checked, 1);
        assert_eq!(rule.comparison, "maximum");
        assert_eq!(rule.subject, "hole");
        assert_eq!(rule.limit.normalized_unit, "ratio");
        assert_eq!(rule.limit.normalized_value, 8.0);
        assert!(rule.assumptions.is_empty());
    }

    #[test]
    fn through_hole_failure_and_report_json_expose_scalar_evidence() {
        let report = run(
            &BOARD.replace("diameter=\"0.2\"", "diameter=\"0.16\""),
            PTH_PDK,
        );
        assert!(matches!(report.verdict, Verdict::Fail));
        let rule = only_rule(&report);
        assert!(matches!(rule.status, RuleStatus::Fail));
        let finding = &report.findings[0];
        assert!(matches!(
            finding.measurement,
            Measurement::Ratio {
                actual_ratio: 10.0,
                maximum_ratio: 8.0,
                drilled_span_thickness_mm: 1.6,
                finished_hole_diameter_mm: 0.16,
                thickness_source: "ipc_2581_overall_thickness",
                ..
            }
        ));
        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["rules"][0]["comparison"], "maximum");
        assert_eq!(json["rules"][0]["subject"], "hole");
        assert_eq!(json["findings"][0]["measurement"]["actual_ratio"], 10.0);
        assert_eq!(json["findings"][0]["measurement"]["maximum_ratio"], 8.0);
        assert_eq!(
            json["findings"][0]["measurement"]["thickness_source"],
            "ipc_2581_overall_thickness"
        );
        assert_eq!(
            json["findings"][0]["subjects"][0]["drill_span"],
            serde_json::json!({
                "first_copper_index": 0,
                "last_copper_index": 3,
                "interpretation": "declared_through_board"
            })
        );
        assert_eq!(
            json["findings"][0]["sites"][0]["measurement_kind"],
            "aspect_ratio"
        );
        assert_eq!(
            json["findings"][0]["sites"][0]["witnesses"],
            serde_json::json!([])
        );
        assert_eq!(json["findings"][0]["evidence"][0]["kind"], "circle");
    }

    #[test]
    fn through_hole_uses_and_reports_profile_default_fallback() {
        let xml = BOARD
            .replace(" overallThickness=\"1.6\"", "")
            .replace(
                "layerOrGroupRef=\"D1\" thickness=\"0.20\"",
                "layerOrGroupRef=\"D1\"",
            )
            .replace("diameter=\"0.2\"", "diameter=\"0.16\"");
        let report = run(&xml, PTH_PDK_WITH_DEFAULT);
        let rule = only_rule(&report);
        assert_eq!(rule.assumptions.len(), 1);
        assert!(rule.assumptions[0].contains("defaults.board_thickness = '1.6 mm'"));
        assert!(matches!(
            report.findings[0].measurement,
            Measurement::Ratio {
                actual_ratio: 10.0,
                thickness_source: "profile_default_board_thickness",
                ..
            }
        ));
    }

    #[test]
    fn through_hole_with_missing_thickness_and_no_default_is_skipped() {
        let xml = BOARD.replace(" overallThickness=\"1.6\"", "").replace(
            "layerOrGroupRef=\"D1\" thickness=\"0.20\"",
            "layerOrGroupRef=\"D1\"",
        );
        let report = run(&xml, PTH_PDK);
        let rule = only_rule(&report);
        assert!(matches!(rule.status, RuleStatus::Skipped));
        assert_eq!(rule.checked, 0);
        assert!(
            rule.skip_reason
                .as_deref()
                .unwrap()
                .contains("physical stackup layer 'D1' has no thickness")
        );
        assert!(report.findings.is_empty());
    }

    #[test]
    fn blind_and_buried_holes_use_only_their_physical_spans() {
        let report = run(BOARD, VIA_PDK_WITH_DEFAULT);
        let rule = only_rule(&report);
        assert_eq!(rule.id, "via-aspect-ratio.4-layer");
        assert_eq!(rule.checked, 2);
        assert_eq!(report.findings.len(), 2);
        let measurements = report
            .findings
            .iter()
            .map(|finding| match finding.measurement {
                Measurement::Ratio {
                    actual_ratio,
                    drilled_span_thickness_mm,
                    thickness_source,
                    ..
                } => (actual_ratio, drilled_span_thickness_mm, thickness_source),
                _ => panic!("expected ratio measurement"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            measurements,
            [
                (10.0, 0.30, "ipc_2581_stackup_layer_thicknesses"),
                (10.0, 0.50, "ipc_2581_stackup_layer_thicknesses"),
            ]
        );
        let spans = report
            .findings
            .iter()
            .map(|finding| {
                let span = finding.subjects[0].drill_span.as_ref().unwrap();
                (span.first_copper_index, span.last_copper_index)
            })
            .collect::<Vec<_>>();
        assert_eq!(spans, [(0, 1), (1, 2)]);
    }

    #[test]
    fn missing_blind_span_thickness_skips_instead_of_using_board_default() {
        let xml = BOARD.replace(
            "layerOrGroupRef=\"D2\" thickness=\"0.40\"",
            "layerOrGroupRef=\"D2\"",
        );
        let report = run(&xml, VIA_PDK_WITH_DEFAULT);
        let rule = only_rule(&report);
        assert!(matches!(report.verdict, Verdict::Pass));
        assert!(matches!(rule.status, RuleStatus::Skipped));
        assert_eq!(rule.checked, 0);
        assert!(rule.assumptions.is_empty());
        assert!(
            rule.skip_reason
                .as_deref()
                .unwrap()
                .contains("physical stackup layer 'D2' has no thickness")
        );
        assert!(report.findings.is_empty());
    }
}
