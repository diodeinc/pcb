use ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{BBox, ContourBuf, PathCmd, Point, Resolution};

use super::data::{self, ArrayData, Chip, DrillTool, Hit, HoleKind, Ink, REQUIREMENTS, Source};
use super::{FabDrawingOptions, SheetSize, fab_drawing};
use crate::LayoutTarget;
use crate::accessors::ColorInfo;
use crate::accessors::IpcAccessor;
use crate::commands::board_array::{
    BoardArrayCreateOptions, BoardMarginMm, Separation, create_board_array,
};

/// A 40 x 30 mm two-layer board: black mask and ENIG on both sides, legend
/// on the top alone, three vias, two component holes, a mounting hole and a
/// plated slot.
const BOARD: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="widget"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="1.8"/></EntryStandard>
      <EntryStandard id="opening"><Circle diameter="2"/></EntryStandard>
      <EntryStandard id="mark"><RectCenter width="6" height="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <HistoryRecord number="1" origination="2026-03-04T05:06:07" software="KiCad EDA" lastChange="2026-03-04T05:06:07">
    <FileRevision fileRevisionId="1" comment="NO COMMENT">
      <SoftwarePackage name="KiCad" revision="10.0.4" vendor="KiCad EDA">
        <Certification certificationStatus="SELFTEST"/>
      </SoftwarePackage>
    </FileRevision>
  </HistoryRecord>
  <Ecad name="widget">
    <CadHeader units="MILLIMETER">
      <Spec name="mask">
        <General type="MATERIAL"><Property text="Color : Black"/></General>
      </Spec>
      <Spec name="legend">
        <General type="MATERIAL"><Property text="Color : White"/></General>
      </Spec>
      <Spec name="core">
        <General type="MATERIAL"><Property text="FR4"/></General>
        <Dielectric type="DIELECTRIC_CONSTANT"><Property value="4.5"/></Dielectric>
      </Spec>
      <Spec name="finish"><SurfaceFinish type="ENIG-N"/></Spec>
    </CadHeader>
    <CadData>
      <Layer name="F.Silkscreen" layerFunction="SILKSCREEN" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Mask" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="Core" layerFunction="DIELCORE" side="INTERNAL" polarity="POSITIVE"/>
      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="B.Mask" layerFunction="SOLDERMASK" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="B.Silkscreen" layerFunction="SILKSCREEN" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="F.Paste" layerFunction="SOLDERPASTE" side="TOP" polarity="POSITIVE"/>
      <Layer name="Finish" layerFunction="COATINGCOND" side="TOP" polarity="POSITIVE"/>
      <Layer name="Drill" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Layer name="Rout" layerFunction="ROUT" side="ALL" polarity="POSITIVE">
        <Span fromLayer="F.Cu" toLayer="B.Cu"/>
      </Layer>
      <Stackup name="Stackup" overallThickness="1.6" whereMeasured="MASK">
        <StackupGroup name="Group">
          <StackupLayer layerOrGroupRef="F.Silkscreen" thickness="0" sequence="0"><SpecRef id="legend"/></StackupLayer>
          <StackupLayer layerOrGroupRef="F.Mask" thickness="0.01" sequence="1"><SpecRef id="mask"/></StackupLayer>
          <StackupLayer layerOrGroupRef="Finish" thickness="0" sequence="2"><SpecRef id="finish"/></StackupLayer>
          <StackupLayer layerOrGroupRef="F.Cu" thickness="0.035" sequence="3"/>
          <StackupLayer layerOrGroupRef="Core" thickness="1.51" sequence="4"><SpecRef id="core"/></StackupLayer>
          <StackupLayer layerOrGroupRef="B.Cu" thickness="0.035" sequence="5"/>
          <StackupLayer layerOrGroupRef="B.Mask" thickness="0.01" sequence="6"><SpecRef id="mask"/></StackupLayer>
          <StackupLayer layerOrGroupRef="B.Silkscreen" thickness="0" sequence="7"><SpecRef id="legend"/></StackupLayer>
        </StackupGroup>
      </Stackup>
      <Step name="widget" type="BOARD">
        <Datum x="0" y="0"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="40" y="0"/>
            <PolyStepSegment x="40" y="30"/>
            <PolyStepSegment x="25" y="30"/>
            <PolyStepSegment x="25" y="24"/>
            <PolyStepSegment x="15" y="24"/>
            <PolyStepSegment x="15" y="30"/>
            <PolyStepSegment x="0" y="30"/>
          </Polygon>
        </Profile>
        <PadStackDef name="top-pad">
          <PadstackPadDef layerRef="F.Cu" padUse="REGULAR"><StandardPrimitiveRef id="pad"/></PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="bottom-pad">
          <PadstackPadDef layerRef="B.Cu" padUse="REGULAR"><StandardPrimitiveRef id="pad"/></PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="top-opening">
          <PadstackPadDef layerRef="F.Mask" padUse="REGULAR"><StandardPrimitiveRef id="opening"/></PadstackPadDef>
        </PadStackDef>
        <PadStackDef name="top-mark">
          <PadstackPadDef layerRef="F.Silkscreen" padUse="REGULAR"><StandardPrimitiveRef id="mark"/></PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="F.Cu">
          <Set><Pad padstackDefRef="top-pad"><Location x="10" y="10"/></Pad></Set>
          <Set><Pad padstackDefRef="top-pad"><Location x="30" y="10"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="B.Cu">
          <Set><Pad padstackDefRef="bottom-pad"><Location x="10" y="10"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="F.Mask">
          <Set><Pad padstackDefRef="top-opening"><Location x="10" y="10"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="F.Silkscreen">
          <Set><Pad padstackDefRef="top-mark"><Location x="20" y="18"/></Pad></Set>
        </LayerFeature>
        <LayerFeature layerRef="Drill">
          <Set><Hole name="V1" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="5" y="5"/></Set>
          <Set><Hole name="V2" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="6" y="5"/></Set>
          <Set><Hole name="V3" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="7" y="5"/></Set>
          <Set><Hole name="P1" diameter="1" platingStatus="PLATED" plusTol="0" minusTol="0" x="10" y="10"/></Set>
          <Set><Hole name="P2" diameter="1" platingStatus="PLATED" plusTol="0" minusTol="0" x="30" y="10"/></Set>
          <Set><Hole name="M1" diameter="3.2" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="35" y="25"/></Set>
        </LayerFeature>
        <LayerFeature layerRef="Rout">
          <Set>
            <SlotCavity name="S1" platingStatus="PLATED" plusTol="0" minusTol="0">
              <Location x="20" y="8"/>
              <Oval width="2.4" height="0.8"/>
            </SlotCavity>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

fn array(separation: Separation) -> String {
    let options = BoardArrayCreateOptions {
        columns: 2,
        rows: 2,
        board_margin_mm: BoardMarginMm::all(5.0),
        edge_rail_mm: BoardMarginMm::all(5.0),
    };
    create_board_array(BOARD, &options, false, separation, Resolution::default())
        .unwrap()
        .xml
}

/// Read `xml` and hand what a drawing reads of it to `inspect`.
fn with_source<T>(xml: &str, inspect: impl FnOnce(&Source<'_>) -> T) -> T {
    let ipc = Ipc2581::parse(xml).unwrap();
    let resolution = Resolution::default();
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
    inspect(&Source {
        ipc: &ipc,
        imported: &imported,
        accessor: IpcAccessor::new(&ipc),
        resolution,
    })
}

fn draw(xml: &str, options: &FabDrawingOptions) -> Vec<u8> {
    let ipc = Ipc2581::parse(xml).unwrap();
    let resolution = Resolution::default();
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
    fab_drawing(&ipc, &imported, options, resolution).unwrap()
}

fn page_count(pdf: &[u8]) -> usize {
    let text = String::from_utf8_lossy(pdf);
    text.matches("/Type /Page\n").count()
}

/// Which layers of the fixture carry artwork: all but the bottom legend.
fn printed(source: &Source<'_>) -> Vec<(data::FabLayer, bool)> {
    data::fab_layers(source.imported)
        .into_iter()
        .map(|layer| {
            let inked = !(layer.role == LayerRole::Legend && layer.side == Side::Bottom);
            (layer, inked)
        })
        .collect()
}

#[test]
fn a_board_draws_its_outline_its_drill_pattern_then_a_sheet_a_layer() {
    let options = FabDrawingOptions::default();
    let pdf = draw(BOARD, &options);
    assert!(pdf.starts_with(b"%PDF-"));
    // The board, its drill pattern with the table beside it, then five
    // layers: the bottom legend has no artwork and paste is not the
    // fabricator's.
    assert_eq!(page_count(&pdf), 7);
    assert_eq!(pdf, draw(BOARD, &options), "a drawing is deterministic");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/MediaBox [0 0 841.8898 595.2756]"), "A4");

    // Asked for more layers to a sheet, they share one at a smaller scale.
    let shared = FabDrawingOptions {
        layers_per_sheet: 6,
        ..options.clone()
    };
    assert_eq!(page_count(&draw(BOARD, &shared)), 3);
    // Left to fit its sheet, a small board still draws on A4.
    let fitted = FabDrawingOptions {
        sheet: None,
        ..options
    };
    assert_eq!(page_count(&draw(BOARD, &fitted)), 7);
    let large = FabDrawingOptions {
        sheet: Some(SheetSize::A2),
        ..FabDrawingOptions::default()
    };
    assert!(page_count(&draw(BOARD, &large)) <= 7);
}

#[test]
fn an_array_adds_its_own_sheet_to_its_boards() {
    let options = FabDrawingOptions::default();
    for separation in [Separation::VScore, Separation::MouseBite] {
        let xml = array(separation);
        assert_eq!(page_count(&draw(&xml, &options)), 8, "{separation:?}");
        // Drawn as a board, an array file is the board's drawing.
        let board = FabDrawingOptions {
            target: LayoutTarget::Board,
            ..options.clone()
        };
        assert_eq!(page_count(&draw(&xml, &board)), 7, "{separation:?}");
    }
}

#[test]
fn the_drill_table_lists_every_opening_smallest_first() {
    with_source(BOARD, |source| {
        let tools = data::drill_tools(source.imported, ArtworkScope::Board).unwrap();
        let rows = tools
            .iter()
            .map(|tool| {
                let size = data::mm_fine(tool.diameter);
                (
                    tool.usage(),
                    size,
                    tool.shape(),
                    tool.hits.len(),
                    tool.span(),
                )
            })
            .collect::<Vec<_>>();
        let thru = || "THRU".to_string();
        assert_eq!(
            rows,
            [
                ("VIA", "0.30".to_string(), None, 3, thru()),
                (
                    "PTH",
                    "0.80".to_string(),
                    Some("SLOT 2.40".to_string()),
                    1,
                    thru()
                ),
                ("PTH", "1.00".to_string(), None, 2, thru()),
                ("NPTH", "3.20".to_string(), None, 1, thru()),
            ]
        );
        // The tools with the most holes take the lightest symbols.
        assert_eq!(super::assign_symbols(&tools), [0, 2, 1, 3]);
    });

    // A slot that is no oval is still an opening: listed by its extents
    // and drawn by its edge, not a reason to draw nothing.
    let routed = BOARD.replace(
        r#"<Oval width="2.4" height="0.8"/>"#,
        r#"<RectCenter width="2.4" height="0.8"/>"#,
    );
    with_source(&routed, |source| {
        let tools = data::drill_tools(source.imported, ArtworkScope::Board).unwrap();
        let slot = tools
            .iter()
            .find(|tool| tool.slot_length.is_some())
            .unwrap();
        assert_eq!(slot.shape().as_deref(), Some("ROUTED PER DATA"));
        assert!(matches!(slot.hits[0], Hit::Routed(_)));
        assert_eq!(slot.hits[0].center(), Point::new(20.0, 8.0));
    });
    assert!(page_count(&draw(&routed, &FabDrawingOptions::default())) > 2);
}

#[test]
fn layers_are_numbered_as_the_stackup_orders_them() {
    let listed = |xml: &str| {
        with_source(xml, |source| {
            data::fab_layers(source.imported)
                .iter()
                .map(|layer| (layer.name.clone(), layer.number, layer.description()))
                .collect::<Vec<_>>()
        })
    };
    let expected = [
        ("F.Silkscreen", None, "TOP LEGEND"),
        ("F.Mask", None, "TOP SOLDER MASK"),
        ("F.Cu", Some(1), "TOP COPPER"),
        ("B.Cu", Some(2), "BOTTOM COPPER"),
        ("B.Mask", None, "BOTTOM SOLDER MASK"),
        ("B.Silkscreen", None, "BOTTOM LEGEND"),
    ]
    .map(|(name, number, description)| (name.to_string(), number, description.to_string()));
    assert_eq!(listed(BOARD), expected);

    // Declared bottom first, the copper is still numbered from the top:
    // the stackup says which layer is which, not the order of the file.
    let top = r#"      <Layer name="F.Cu" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
"#;
    let bottom = r#"      <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
"#;
    assert!(BOARD.contains(top) && BOARD.contains(bottom));
    let swapped = BOARD
        .replace(top, "")
        .replace(bottom, &format!("{bottom}{top}"));
    assert_eq!(listed(&swapped), expected);
}

#[test]
fn the_specification_states_inks_and_finish_with_a_chip_for_each_side() {
    with_source(BOARD, |source| {
        let tools = data::drill_tools(source.imported, ArtworkScope::Board).unwrap();
        let board = BBox::new(Point::ZERO, Point::new(40.0, 30.0));
        let rows = data::specification(source, &printed(source), &tools, board, None).unwrap();
        let row = |label: &str| {
            let row = rows.iter().find(|row| row.label == label);
            row.unwrap_or_else(|| panic!("no {label} row in {rows:?}"))
        };
        assert_eq!(row("BOARD SIZE").value, "40.00 × 30.00 mm");
        assert_eq!(row("COPPER LAYERS").value, "2");
        // The fixture states no tolerance, so the drawing invents none.
        assert_eq!(row("THICKNESS").value, "1.60 mm OVER MASK");
        assert_eq!(row("MATERIAL").value, "FR4");
        assert_eq!(row("COPPER").value, "1 oz");
        assert_eq!(row("SURFACE FINISH").value, "ENIG");
        assert!(matches!(row("SURFACE FINISH").chips[..], [Chip::Color(_)]));
        assert_eq!(row("SOLDER MASK").value, "BLACK · BOTH SIDES");
        assert!(matches!(
            row("SOLDER MASK").chips[..],
            [Chip::Color(0x1c1c1c), Chip::Color(0x1c1c1c)]
        ));
        // A legend layer with nothing on it prints nothing.
        assert_eq!(row("LEGEND").value, "WHITE · TOP ONLY");
        assert!(matches!(
            row("LEGEND").chips[..],
            [Chip::Color(_), Chip::Absent]
        ));
        assert_eq!(row("HOLES").value, "7 · 4 SIZES · MIN Ø0.30 · ASPECT 5.3:1");
        // No mask opening lies over a via on either side.
        assert_eq!(row("VIAS").value, "3 · TENTED BOTH SIDES");
        assert_eq!(row("SPECIAL").value, "PLATED SLOTS");
        assert!(rows.iter().all(|row| row.label != "MIN TRACK"), "no tracks");
    });
}

#[test]
fn an_ink_is_shown_as_far_as_the_design_states_it() {
    let named = |name: &str| ColorInfo {
        name: Some(name.to_string()),
        rgb: None,
    };
    assert_eq!(
        Ink::of(Some(&named("Green"))),
        Ink {
            color: Some(0x1a6b3a),
            name: "GREEN".to_string()
        }
    );
    // A colour nobody chose, and no colour at all.
    for unstated in [Some(named("Not specified")), None] {
        let ink = Ink::of(unstated.as_ref());
        assert_eq!((ink.color, ink.name.as_str()), (None, "NOT SPECIFIED"));
    }
    // A colour mixed on screen: its chip is that colour, alpha dropped.
    let custom = Ink::of(Some(&named("#CC66004A")));
    assert_eq!(
        (custom.color, custom.name.as_str()),
        (Some(0xcc6600), "PER DATA #CC6600")
    );
}

#[test]
fn only_a_few_notes_are_kept_and_an_array_adds_one() {
    let notes = data::notes(None, &REQUIREMENTS);
    assert_eq!(notes.len(), 4);
    assert!(notes[0].contains("IPC-6012 / IPC-A-600 CLASS 2"));
    with_source(&array(Separation::VScore), |source| {
        let array = ArrayData::of(source).unwrap().unwrap();
        let notes = data::notes(Some(&array), &REQUIREMENTS);
        assert_eq!(notes.len(), 5);
        assert!(notes[4].contains("DO NOT MOVE SCORES"));
    });
}

#[test]
fn an_array_says_how_it_is_separated() {
    with_source(&array(Separation::VScore), |source| {
        let array = ArrayData::of(source).unwrap().unwrap();
        assert_eq!(array.boards.len(), 4);
        assert_eq!((array.bounds.width(), array.bounds.height()), (110.0, 90.0));
        assert!(!array.scores.is_empty());
        assert!(array.tab().is_none(), "a scored array has no tabs");
        assert!(array.separation().starts_with("V-SCORE"));
    });
    with_source(&array(Separation::MouseBite), |source| {
        let mut array = ArrayData::of(source).unwrap().unwrap();
        assert!(array.scores.is_empty() && !array.removal.is_empty());
        assert_eq!(array.separation(), "ROUTED, PERFORATED TABS");
        let tab = array.tab().expect("a routed array is held by tabs");
        assert!(tab.holes >= 3, "{tab:?}");
        assert!(tab.pitch > tab.diameter, "perforations do not overlap");

        // Without its perforations the array's smallest holes are its
        // tooling, which stand too far apart to be a tab.
        array.tools.retain(|tool| tool.diameter != tab.diameter);
        assert!(array.tab().is_none());
        assert_eq!(array.separation(), "ROUTED, TABS");
    });
}

#[test]
fn holes_far_apart_are_not_a_tab() {
    let square = ContourBuf::new(vec![
        PathCmd::move_to(Point::ZERO),
        PathCmd::line_to(Point::new(1.0, 0.0)),
        PathCmd::line_to(Point::new(1.0, 1.0)),
        PathCmd::close(),
    ]);
    let tool = |pitch: f64| DrillTool {
        diameter: 0.5,
        slot_length: None,
        kind: HoleKind::NonPlated,
        layers: (1, 2),
        layer_count: 2,
        hits: (0..4)
            .map(|hole| Hit::Hole(Point::new(f64::from(hole) * pitch, 0.0)))
            .collect(),
    };
    let array = |pitch: f64| ArrayData {
        grid: None,
        bounds: BBox::new(Point::ZERO, Point::new(100.0, 100.0)),
        boards: Vec::new(),
        outlines: Vec::new(),
        removal: vec![square.clone()],
        scores: Vec::new(),
        fiducials: Vec::new(),
        tools: vec![tool(pitch)],
    };
    assert_eq!(array(0.8).tab().unwrap().holes, 4);
    assert!(array(20.0).tab().is_none());
}

#[test]
fn a_board_file_has_no_array_to_draw() {
    with_source(BOARD, |source| {
        assert!(ArrayData::of(source).unwrap().is_none());
        assert_eq!(data::design_name(source), "widget");
        assert_eq!(data::design_date(source).as_deref(), Some("2026-03-04"));
        assert_eq!(data::design_revision(source), None);
    });
}

#[test]
fn copper_is_named_by_the_foil_weight_it_is_sold_by() {
    assert_eq!(data::copper_weight(0.035), "1 oz");
    assert_eq!(data::copper_weight(0.0175), "1/2 oz");
    assert_eq!(data::copper_weight(0.07), "2 oz");
    // Nobody stocks 0.86 oz foil.
    assert_eq!(data::copper_weight(0.030), "30 µm");
    assert_eq!(data::mm_fine(12.196), "12.196");
    assert_eq!(data::mm_fine(74.0), "74.00");
    assert_eq!(data::mm_fine(-0.0001), "0.00");
    assert_eq!(data::mm(12.196), "12.20");
}
