use ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::dialects::{LayerRole, Side};
use pcb_ir::geom::{BBox, Point, Resolution};

use super::data::{self, ArrayData, Chip, DrillTool, Hit, HoleKind, Source};
use super::{FabDrawingOptions, fab_drawing};
use crate::LayoutTarget;
use crate::commands::board_array::{
    BoardArrayCreateOptions, BoardMarginMm, Separation, create_board_array,
};

/// A 40 x 30 mm two-layer board: black mask and ENIG on both sides, top legend
/// only, three vias, two component holes, a mounting hole and a plated slot.
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

fn with_source<T>(xml: &str, inspect: impl FnOnce(&Source<'_>) -> T) -> T {
    let ipc = Ipc2581::parse(xml).unwrap();
    let resolution = Resolution::default();
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution).unwrap();
    inspect(&Source::new(&ipc, &imported, resolution))
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
fn a_board_draws_its_outline_its_drill_pattern_then_two_layers_a_sheet() {
    let options = FabDrawingOptions::default();
    let pdf = draw(BOARD, &options);
    assert!(pdf.starts_with(b"%PDF-"));
    // The board, its drill pattern, then five layers two to a sheet.
    assert_eq!(page_count(&pdf), 5);
    assert_eq!(pdf, draw(BOARD, &options), "a drawing is deterministic");
    let text = String::from_utf8_lossy(&pdf);
    assert!(text.contains("/MediaBox [0 0 841.8898 595.2756]"), "A4");
}

#[test]
fn an_array_adds_its_own_sheets_to_its_boards() {
    let options = FabDrawingOptions::default();
    for separation in [Separation::VScore, Separation::MouseBite] {
        let xml = array(separation);
        // The array, its tooling with each side's fiducials, and after the
        // board's layers the array's own.
        assert_eq!(page_count(&draw(&xml, &options)), 11, "{separation:?}");
        // Drawn as a board, an array file is the board's drawing.
        let board = FabDrawingOptions {
            target: LayoutTarget::Board,
            ..options.clone()
        };
        assert_eq!(page_count(&draw(&xml, &board)), 5, "{separation:?}");
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
        assert_eq!(
            rows,
            [
                ("VIA", "0.30".to_string(), None, 3, None),
                (
                    "PTH",
                    "0.80".to_string(),
                    Some("SLOT 0.80 × 2.40".to_string()),
                    1,
                    None
                ),
                ("PTH", "1.00".to_string(), None, 2, None),
                ("NPTH", "3.20".to_string(), None, 1, None),
            ]
        );
        assert_eq!(super::assign_symbols(&tools), [0, 2, 1, 3]);
    });

    // A slot that is no oval is listed by its extents and drawn by its edge.
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

    // Declared bottom first, the copper is still numbered from the top.
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
fn the_specification_states_inks_and_finish_with_one_chip_each() {
    with_source(BOARD, |source| {
        let tools = data::drill_tools(source.imported, ArtworkScope::Board).unwrap();
        let board = BBox::new(Point::ZERO, Point::new(40.0, 30.0));
        let rows = data::specification(source, &printed(source), &tools, board, None);
        let row = |label: &str| {
            let row = rows.iter().find(|row| row.label == label);
            row.unwrap_or_else(|| panic!("no {label} row in {rows:?}"))
        };
        assert_eq!(row("BOARD SIZE").value, "40.00 × 30.00 mm");
        assert_eq!(row("COPPER LAYERS").value, "2");
        assert_eq!(row("THICKNESS").value, "1.60 mm OVER MASK");
        assert_eq!(row("COPPER").value, "1 oz");
        assert_eq!(row("SURFACE FINISH").value, "ENIG");
        assert!(matches!(row("SURFACE FINISH").chip, Some(Chip::Color(_))));
        assert_eq!(row("SOLDER MASK").value, "BLACK · BOTH SIDES");
        assert_eq!(row("SOLDER MASK").chip, Some(Chip::Color(0x1c1c1c)));
        // A legend layer with nothing on it prints nothing.
        assert_eq!(row("LEGEND").value, "WHITE · TOP ONLY");
        assert_eq!(row("LEGEND").chip, Some(Chip::Color(0xf4f4f0)));
        assert_eq!(row("HOLES").value, "7 · 4 SIZES · MIN DIA 0.30");
    });
}

#[test]
fn a_design_that_names_no_ink_is_built_green_with_a_white_legend() {
    let unnamed = BOARD
        .replace(r#"<Property text="Color : Black"/>"#, "")
        .replace(r#"<Property text="Color : White"/>"#, "");
    assert_ne!(unnamed, BOARD);
    with_source(&unnamed, |source| {
        let ink = |role, side| data::layer_ink(source, role, side).unwrap();
        for side in [Side::Top, Side::Bottom] {
            let mask = ink(LayerRole::Soldermask, side);
            assert_eq!((mask.color, mask.name.as_str()), (Some(0x1a6b3a), "GREEN"));
            let legend = ink(LayerRole::Legend, side);
            assert_eq!(
                (legend.color, legend.name.as_str()),
                (Some(0xf4f4f0), "WHITE")
            );
        }
    });
    // Named on one side only, an ink is the board's: both sides have it.
    let bottom = r#"<StackupLayer layerOrGroupRef="B.Mask" thickness="0.01" sequence="6"><SpecRef id="mask"/></StackupLayer>"#;
    assert!(BOARD.contains(bottom));
    let one_sided = BOARD.replace(bottom, &bottom.replace(r#"<SpecRef id="mask"/>"#, ""));
    with_source(&one_sided, |source| {
        let mask = data::layer_ink(source, LayerRole::Soldermask, Side::Bottom).unwrap();
        assert_eq!(mask.name, "BLACK");
    });
}

#[test]
fn an_array_says_how_it_is_separated() {
    with_source(&array(Separation::VScore), |source| {
        let array = ArrayData::of(source).unwrap().unwrap();
        assert_eq!(array.boards.len(), 4);
        assert_eq!((array.bounds.width(), array.bounds.height()), (110.0, 90.0));
        assert!(!array.scores.is_empty());
        assert!(array.tab.is_none(), "a scored array has no tabs");
        assert!(array.separation().starts_with("V-SCORE"));
    });
    with_source(&array(Separation::MouseBite), |source| {
        let array = ArrayData::of(source).unwrap().unwrap();
        assert!(array.scores.is_empty() && !array.removal.is_empty());
        assert_eq!(array.separation(), "ROUTED, PERFORATED TABS");
        let tab = array.tab.expect("a routed array is held by tabs");
        assert!(tab.holes >= 3, "{tab:?}");
        assert!(tab.pitch > tab.diameter, "perforations do not overlap");
        // No perforation is taken for a tooling hole.
        let mut tooling = array.tooling.iter();
        assert!(tooling.all(|(_, tool)| array.tools[*tool].diameter != tab.diameter));
    });
}

#[test]
fn an_array_places_its_tooling_and_tells_its_fiducials_from_its_boards() {
    with_source(&array(Separation::VScore), |source| {
        let array = ArrayData::of(source).unwrap().unwrap();
        let tooling = &array.tooling;
        assert!(tooling.len() >= 3, "{}", tooling.len());
        assert!(
            tooling
                .iter()
                .all(|(at, _)| array.bounds.contains_point(*at))
        );
        let own = array
            .fiducials
            .iter()
            .filter(|fiducial| fiducial.board.is_none());
        let own = own.collect::<Vec<_>>();
        assert!(!own.is_empty());
        for fiducial in own {
            let mut boards = array.boards.iter();
            assert!(
                !boards.any(|board| board.contains_point(fiducial.at)),
                "{fiducial:?}"
            );
        }
        // Every board has the same fiducials, so each is listed once a side.
        for side in [Side::Top, Side::Bottom] {
            let cells = array.fiducials.iter();
            let cells = cells.filter(|fiducial| fiducial.board.is_some() && fiducial.side == side);
            let per_board = array.board_fiducials(side);
            assert!(!per_board.is_empty(), "{side:?}");
            assert_eq!(per_board.len() * array.boards.len(), cells.count());
        }
    });
}

#[test]
fn perforations_are_rows_and_holes_apart_are_tooling() {
    let tool = |pitch: f64| DrillTool {
        diameter: 0.5,
        slot_length: None,
        kind: HoleKind::NonPlated,
        layers: (1, 2),
        layer_count: 2,
        hits: (0..4)
            .map(|hole| Hit::Hole(Point::new(f64::from(hole) * pitch, 0.0)))
            .chain([Hit::Hole(Point::new(50.0, 50.0))])
            .collect(),
    };
    // A row of four, and a hole of the same size standing apart from it.
    let (rows, apart) = data::perforations(&tool(0.8));
    assert_eq!((rows.len(), rows[0].len()), (1, 4));
    assert_eq!(apart, [Point::new(50.0, 50.0)]);
    let (rows, apart) = data::perforations(&tool(20.0));
    assert!(rows.is_empty());
    assert_eq!(apart.len(), 5);
}
