//! Package-wide coordinates, independently checked against hand-placed artwork.
use pcb_ipc2581_tools::manufacturing::{ManufacturingExportOptions, build_manufacturing_package};
use pcb_ir::dialects::{artwork, ipc::ArtworkScope};
use pcb_ir::geom::Resolution;
use pcb_ir::import::ipc2581::import_design;
use std::collections::BTreeMap;

const LAYERS: [(&str, &str, &str); 8] = [
    ("SIGNAL", "TOP", "F_Cu.gtl"),
    ("SIGNAL", "BOTTOM", "B_Cu.gbl"),
    ("SOLDERMASK", "TOP", "F_Mask.gts"),
    ("SOLDERMASK", "BOTTOM", "B_Mask.gbs"),
    ("SOLDERPASTE", "TOP", "F_Paste.gtp"),
    ("SOLDERPASTE", "BOTTOM", "B_Paste.gbp"),
    ("LEGEND", "TOP", "F_SilkS.gto"),
    ("LEGEND", "BOTTOM", "B_SilkS.gbo"),
];

fn fixture(units: &str, nested: bool) -> ipc2581::Ipc2581 {
    // Inch source coordinates and the primitive dictionary use different
    // units. Values below describe the same physical board in either input unit.
    let divisor = if units == "INCH" { 25.4 } else { 1.0 };
    let n = |mm: f64| mm / divisor;
    let rectangle = |tag, x0, y0, x1, y1| {
        format!(
            r#"<{tag}>
      <PolyBegin x="{}" y="{}"/><PolyStepSegment x="{}" y="{}"/>
      <PolyStepSegment x="{}" y="{}"/><PolyStepSegment x="{}" y="{}"/>
      <PolyStepSegment x="{}" y="{}"/></{tag}>"#,
            n(x0),
            n(y0),
            n(x1),
            n(y0),
            n(x1),
            n(y1),
            n(x0),
            n(y1),
            n(x0),
            n(y0)
        )
    };
    let board_profile =
        rectangle("Polygon", -8.0, -6.0, 12.0, 11.0) + &rectangle("Cutout", -7.0, -4.0, -5.0, -1.0);
    let panel_profile = rectangle("Polygon", -30.0, -25.0, 35.0, 5.0);
    let outer_profile = rectangle("Polygon", -50.0, -70.0, 0.0, 70.0);
    let mut layers = String::new();
    let mut features = String::new();
    for (i, (function, side, _)) in LAYERS.iter().enumerate() {
        layers += &format!(
            r#"<Layer name="L{i}" layerFunction="{function}" side="{side}" polarity="POSITIVE"/>"#
        );
        // Keep copper clear of the route: slot artwork policy is a separate
        // contract from package coordinates.
        let large = if *function == "SIGNAL" {
            String::new()
        } else {
            format!(
                r#"<Pad><Location x="{}" y="{}"/><StandardPrimitiveRef id="large"/></Pad>"#,
                n(5.0),
                n(-2.0)
            )
        };
        features += &format!(
            r#"<LayerFeature layerRef="L{i}"><Set>
          <Pad><Location x="{}" y="{}"/><StandardPrimitiveRef id="small"/></Pad>
          {large}
        </Set></LayerFeature>"#,
            n(-3.0),
            n(7.0)
        );
    }
    let panel = format!(
        r#"<Step name="panel" type="PALLET">
      <Datum x="{}" y="{}"/>
      <Profile>{panel_profile}</Profile>
      <StepRepeat stepRef="board" x="{}" y="{}" nx="2" ny="1" dx="{}" dy="0" angle="90" mirror="true"/>
    </Step>"#,
        n(-2.0),
        n(3.0),
        n(20.0),
        n(-10.0),
        n(-30.0)
    );
    let outer = if nested {
        format!(
            r#"<Step name="outer" type="PALLET">
          <Profile>{outer_profile}</Profile>
          <StepRepeat stepRef="panel" x="{}" y="{}" nx="1" ny="2" dx="0" dy="{}" angle="90"/>
        </Step>"#,
            n(-40.0),
            n(30.0),
            n(-70.0)
        )
    } else {
        String::new()
    };
    let root = if nested { "outer" } else { "panel" };
    ipc2581::Ipc2581::parse(&format!(r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
      <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="{root}"/>
        <DictionaryStandard units="MILLIMETER">
          <EntryStandard id="small"><Circle diameter="2"/></EntryStandard>
          <EntryStandard id="large"><Circle diameter="4"/></EntryStandard>
        </DictionaryStandard>
      </Content><Ecad><CadHeader units="{units}"/><CadData>{layers}
        <Layer name="DRILL" layerFunction="DRILL" side="ALL"><Span fromLayer="L0" toLayer="L1"/></Layer>
        <Layer name="ROUTE" layerFunction="ROUT" side="ALL"><Span fromLayer="L0" toLayer="L1"/></Layer>
        <Step name="board" type="BOARD"><Datum x="{}" y="{}"/>
          <Profile>{board_profile}</Profile>{features}
          <LayerFeature layerRef="DRILL"><Set>
            <Hole name="H" diameter="{}" platingStatus="PLATED" plusTol="0" minusTol="0" x="{}" y="{}"/>
          </Set></LayerFeature>
          <LayerFeature layerRef="ROUTE"><Set>
            <SlotCavity name="S" platingStatus="PLATED" plusTol="0" minusTol="0">
              <Location x="{}" y="{}"/><Xform rotation="90" scale="2"/>
              <Oval width="{}" height="{}"/>
            </SlotCavity>
          </Set></LayerFeature>
        </Step>{panel}{outer}
      </CadData></Ecad></IPC-2581>"#,
        n(1.0), n(2.0),
        n(0.4), n(-3.0), n(7.0), n(5.0), n(-2.0), n(2.0), n(0.6),
    )).unwrap()
}

fn image(source: &str) -> gerberx2::geometry::GerberArtworkDocument {
    gerberx2::geometry::extract_document(
        &gerberx2::GerberX2::parse(source).unwrap(),
        Resolution::default().accuracy,
    )
    .unwrap()
}

fn reference(points: &[(f64, f64)], diameter: f64, aperture: u32) -> String {
    let mut result = format!("%ADD{aperture}C,{diameter}*%\nD{aperture}*\n");
    for &(x, y) in points {
        result += &format!(
            "X{}Y{}D03*\n",
            (x * 1e6).round() as i64,
            (y * 1e6).round() as i64
        );
    }
    result
}

#[test]
fn package_keeps_ipc_frame_for_all_layers_and_nc_in_boards_and_arrays() {
    // Expectations are hand-calculated, not taken from the importer or its
    // placement helper. The board Datum is used only when placing a repeat.
    for units in ["MILLIMETER", "INCH"] {
        for (scope, nested, holes, slots) in [
            (
                ArtworkScope::Board,
                true,
                vec![(-3.0, 7.0)],
                vec![(5.0, -2.0)],
            ),
            (
                ArtworkScope::ArrayFlattened,
                false,
                vec![(25.0, -14.0), (-5.0, -14.0)],
                vec![(16.0, -6.0), (-14.0, -6.0)],
            ),
            (
                ArtworkScope::ArrayFlattened,
                true,
                vec![(-23.0, 57.0), (-23.0, 27.0), (-23.0, -13.0), (-23.0, -43.0)],
                vec![(-31.0, 48.0), (-31.0, 18.0), (-31.0, -22.0), (-31.0, -52.0)],
            ),
        ] {
            let source = fixture(units, nested);
            let imported = import_design(&source, Resolution::default()).unwrap();
            let package = build_manufacturing_package(
                &imported,
                &ManufacturingExportOptions {
                    view: scope,
                    relief_debug_dir: None,
                },
                Resolution::default(),
            )
            .unwrap();
            let files: BTreeMap<_, _> = package
                .files
                .into_iter()
                .map(|f| (f.filename, f.contents))
                .collect();
            for (function, _, filename) in LAYERS {
                let mut expected_source =
                    "%FSLAX46Y46*%\n%MOMM*%\n".to_owned() + &reference(&holes, 2.0, 10);
                if function != "SIGNAL" {
                    expected_source += &reference(&slots, 4.0, 11);
                }
                let mut expected = image(&(expected_source + "M02*\n"));
                let actual = &files[filename];
                assert!(actual.contains("%MOMM*%"));
                let actual = image(actual);
                // Compare geometry only; this test does not specify X2 metadata.
                expected.layers[0].meta = actual.layers[0].meta.clone();
                let report = artwork::compare::compare_documents(
                    &expected,
                    &actual,
                    artwork::compare::CompareTolerance {
                        bbox_mm: 0.00001,
                        area_mm2: 0.001,
                    },
                    Resolution::default(),
                )
                .unwrap();
                assert!(
                    report.is_match(),
                    "{units} {scope:?} nested={nested} {filename}: {report:?}"
                );
            }
            let nc = &files["PTH.drl"];
            assert!(nc.contains("METRIC\n"));
            // G90 chooses absolute rather than incremental addressing, not
            // a design origin. The signed positions below must stay IPC-local.
            assert!(nc.contains("%\nG90\n"));
            assert!(nc.contains("C0.4\n"));
            assert!(nc.contains("C1.2\n"));
            let xy = |x: f64, y: f64| format!("X{x:.1}Y{y:.1}");
            let hits: Vec<_> = nc
                .lines()
                .filter(|l| l.starts_with('X') && !l.contains("G85"))
                .collect();
            assert_eq!(hits.len(), holes.len());
            for &(x, y) in &holes {
                assert!(hits.contains(&xy(x, y).as_str()), "{nc}");
            }
            let routes: Vec<_> = nc.lines().filter(|l| l.contains("G85")).collect();
            assert_eq!(routes.len(), slots.len());
            for &(x, y) in &slots {
                let (dx, dy) = if scope == ArtworkScope::ArrayFlattened && !nested {
                    (1.4, 0.0)
                } else {
                    (0.0, 1.4)
                };
                let a = xy(x - dx, y - dy);
                let b = xy(x + dx, y + dy);
                assert!(
                    routes.contains(&format!("{a}G85{b}").as_str())
                        || routes.contains(&format!("{b}G85{a}").as_str()),
                    "{nc}"
                );
            }
            let (filename, rectangles) = if scope == ArtworkScope::Board {
                (
                    "Edge_Cuts.gm1",
                    vec![(-8.0, -6.0, 12.0, 11.0), (-7.0, -4.0, -5.0, -1.0)],
                )
            } else if !nested {
                (
                    "Board_Array_Profile.gm1",
                    vec![
                        (-30.0, -25.0, 35.0, 5.0),
                        (14.0, -18.0, 17.0, -16.0),
                        (-16.0, -18.0, -13.0, -16.0),
                    ],
                )
            } else {
                (
                    "Board_Array_Profile.gm1",
                    vec![
                        (-50.0, -70.0, 0.0, 70.0),
                        (-21.0, 46.0, -19.0, 49.0),
                        (-21.0, 16.0, -19.0, 19.0),
                        (-21.0, -24.0, -19.0, -21.0),
                        (-21.0, -54.0, -19.0, -51.0),
                    ],
                )
            };
            let parsed = gerberx2::GerberX2::parse(&files[filename]).unwrap();
            // Winding and command order may vary, but every complete edge
            // must occur exactly once. Start points alone cannot prove closure.
            let edge = |a: (f64, f64), b: (f64, f64)| if a < b { (a, b) } else { (b, a) };
            let mut edges = Vec::new();
            for object in parsed.objects() {
                let gerberx2::ObjectKind::Draw { start, end, .. } = object.kind else {
                    panic!("unexpected profile object: {:?}", object.kind);
                };
                edges.push(edge((start.x, start.y), (end.x, end.y)));
            }
            let mut expected = Vec::new();
            for (x0, y0, x1, y1) in rectangles {
                let [a, b, c, d] = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
                expected.extend([edge(a, b), edge(b, c), edge(c, d), edge(d, a)]);
            }
            edges.sort_by(|a, b| a.partial_cmp(b).unwrap());
            expected.sort_by(|a, b| a.partial_cmp(b).unwrap());
            assert_eq!(edges, expected, "{units} {scope:?} nested={nested} profile");
        }
    }
}
