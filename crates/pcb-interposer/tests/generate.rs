//! End-to-end: a board-array style panel in, a parseable interposer out.

use pcb_interposer::{Bed, Layers};
use pcb_sexpr::SexprKind;

/// A miniature "panel": A7-sized rounded-rect profile, two NPTH tooling
/// holes (one exactly on an A7-tile corner spot, which must dedupe) and
/// one mouse-bite perforation (which must be left out), one
/// global fiducial per face, and a second top fiducial sitting on a tile
/// corner spot (which must yield to the tile hole).
const PANEL: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="array"/>
    <DictionaryStandard units="MILLIMETER"/>
  </Content>
  <Bom name="BOM_board">
    <BomHeader assembly="board" revision="1.0">
      <StepRef name="array"/>
    </BomHeader>
    <BomItem OEMDesignNumberRef="TP_DP" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP1" packageRef="TestPoint_ICT" populate="true" layerRef="BOTTOM"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Ict" textualCharacteristicValue="usb_dp"/>
        <Textual textualCharacteristicName="Path" textualCharacteristicValue="TP_DP.TP"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TP_DM" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP2" packageRef="TestPoint_ICT" populate="true" layerRef="BOTTOM"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Ict" textualCharacteristicValue="usb_dm"/>
        <Textual textualCharacteristicName="Path" textualCharacteristicValue="TP_DM.TP"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TP_VT" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP3" packageRef="TestPoint_ICT" populate="true" layerRef="BOTTOM"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Ict" textualCharacteristicValue="vtarget"/>
        <Textual textualCharacteristicName="Path" textualCharacteristicValue="TP_VT.TP"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TP_VU" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP4" packageRef="TestPoint_ICT" populate="true" layerRef="BOTTOM"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Ict" textualCharacteristicValue="vusb"/>
        <Textual textualCharacteristicName="Path" textualCharacteristicValue="TP_VU.TP"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TP_G" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP5" packageRef="TestPoint_ICT" populate="true" layerRef="BOTTOM"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Ict" textualCharacteristicValue="gnd"/>
        <Textual textualCharacteristicName="Path" textualCharacteristicValue="TP_G.TP"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TP_SWDIO" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP6" packageRef="TestPoint_ICT" populate="true" layerRef="BOTTOM"/>
      <Characteristics category="ELECTRICAL">
        <Textual textualCharacteristicName="Ict" textualCharacteristicValue="swdio"/>
        <Textual textualCharacteristicName="Path" textualCharacteristicValue="TP_SWDIO.TP"/>
      </Characteristics>
    </BomItem>
  </Bom>
  <Ecad name="ecad">
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="CONDUCTOR" side="TOP" polarity="POSITIVE"/>
      <Layer name="BOTTOM" layerFunction="CONDUCTOR" side="BOTTOM" polarity="POSITIVE"/>
      <Layer name="Board_Array_Drill" layerFunction="DRILL" polarity="POSITIVE"/>
      <Stackup name="stackup" overallThickness="1.6" tolPlus="0.0" tolMinus="0.0">
        <StackupGroup name="group" thickness="1.6" tolPlus="0.0" tolMinus="0.0"/>
      </Stackup>
      <Step name="layout">
        <Datum x="0.0" y="0.0"/>
        <Component refDes="TP1" layerRef="BOTTOM" part="TP_DP" mountType="SMT">
          <Location x="5.0" y="5.0"/>
        </Component>
        <Component refDes="TP2" layerRef="BOTTOM" part="TP_DM" mountType="SMT">
          <Location x="7.0" y="5.0"/>
        </Component>
        <Component refDes="TP3" layerRef="BOTTOM" part="TP_VT" mountType="SMT">
          <Location x="9.0" y="5.0"/>
        </Component>
        <Component refDes="TP4" layerRef="BOTTOM" part="TP_VU" mountType="SMT">
          <Location x="11.0" y="5.0"/>
        </Component>
        <Component refDes="TP5" layerRef="BOTTOM" part="TP_G" mountType="SMT">
          <Location x="13.0" y="5.0"/>
        </Component>
        <Component refDes="TP6" layerRef="BOTTOM" part="TP_SWDIO" mountType="SMT">
          <Location x="15.0" y="5.0"/>
        </Component>
      </Step>
      <Step name="board_cell">
        <Datum x="0.0" y="0.0"/>
        <StepRepeat stepRef="layout" x="2.0" y="3.0" nx="1" ny="1" dx="0" dy="0" angle="0.00" mirror="false"/>
      </Step>
      <Step name="array">
        <Datum x="0.0" y="0.0"/>
        <StepRepeat stepRef="board_cell" x="10.0" y="10.0" nx="1" ny="2" dx="0" dy="40.0" angle="0.00" mirror="false"/>
        <Profile>
          <Polygon>
            <PolyBegin x="0.0" y="3.0"/>
            <PolyStepSegment x="0.0" y="102.0"/>
            <PolyStepCurve x="3.0" y="105.0" centerX="3.0" centerY="102.0" clockwise="true"/>
            <PolyStepSegment x="71.0" y="105.0"/>
            <PolyStepCurve x="74.0" y="102.0" centerX="71.0" centerY="102.0" clockwise="true"/>
            <PolyStepSegment x="74.0" y="3.0"/>
            <PolyStepCurve x="71.0" y="0.0" centerX="71.0" centerY="3.0" clockwise="true"/>
            <PolyStepSegment x="3.0" y="0.0"/>
            <PolyStepCurve x="0.0" y="3.0" centerX="3.0" centerY="3.0" clockwise="true"/>
          </Polygon>
        </Profile>
        <LayerFeature layerRef="Board_Array_Drill">
          <Set polarity="POSITIVE">
            <Hole name="t0" type="CIRCLE" diameter="2.1" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="3.0" y="102.0"/>
            <Hole name="t1" type="CIRCLE" diameter="2.0" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="20.0" y="2.5"/>
            <Hole name="t2" type="CIRCLE" diameter="0.381" platingStatus="NONPLATED" plusTol="0" minusTol="0" x="30.0" y="40.0"/>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="TOP">
          <Set polarity="POSITIVE">
            <GlobalFiducial>
              <Location x="10.0" y="3.85"/>
              <Circle diameter="1"/>
            </GlobalFiducial>
            <GlobalFiducial>
              <Location x="71.5" y="3.5"/>
              <Circle diameter="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="BOTTOM">
          <Set polarity="POSITIVE">
            <GlobalFiducial>
              <Location x="11.0" y="3.85"/>
              <Circle diameter="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#;

#[test]
fn generates_a_parseable_interposer() {
    let ipc = ipc2581::Ipc2581::parse(PANEL).expect("panel fixture parses");
    let panel = pcb_interposer::panel::extract(&ipc).expect("panel extracts");

    assert_eq!((panel.width, panel.height), (74.0, 105.0));
    // The hole at (3, 102) Y-up is (3, 3) Y-down — an exact A7-tile corner
    // spot, so only three tile holes are added on top of the panel's two.
    assert_eq!(panel.holes.len(), 5);
    // The fiducial near the (71, 102) tile corner hole is dropped; the
    // tile is the fixture contract.
    assert_eq!(panel.fids_top.len(), 1);
    assert_eq!(panel.fids_bottom.len(), 1);
    assert_eq!(panel.outline.len(), 8);

    let lands = pcb_interposer::pattern::oriented_s13(panel.width, panel.height);
    let contacts =
        pcb_interposer::contacts::extract_contacts(&ipc, panel.height).expect("contacts");
    let plan = pcb_interposer::plan::plan(contacts, &lands, &[], Bed::S13).expect("plan");
    let text = pcb_interposer::emit::board(&panel, &lands, Bed::S13, Some(&plan), &[], Layers::Two);
    let root = pcb_sexpr::parse(&text).expect("board parses");
    let SexprKind::List(items) = &root.kind else {
        panic!("root is a list");
    };
    let count = |name: &str| {
        items
            .iter()
            .filter(|item| {
                matches!(&item.kind, SexprKind::List(children)
                    if children.first().and_then(|c| c.as_atom()) == Some(name))
            })
            .count()
    };
    // 5 holes + 2 fids + 156 lands + 12 pogos (2 boards × 6 contacts).
    assert_eq!(count("footprint"), 175);
    assert_eq!(count("zone"), 2);
    // Stitch vias are the route step's job; the emitted board has none.
    assert_eq!(count("via"), 0);
    // The four corner mate-detect pairs come pre-shorted.
    assert_eq!(count("segment"), 4);
    assert_eq!(
        text.matches("\"DETECT_").count(),
        12,
        "two pads + their shorting track, x4"
    );
    assert_eq!(count("gr_arc"), 4);
    assert_eq!(count("gr_line"), 4);
    // Nets are named where they are used; the file carries no net table.
    assert_eq!(count("net"), 0);
    // GND: 60 lands, 2 gnd pogos, and the two zones.
    assert_eq!(text.matches("(net \"GND\")").count(), 64);
    // Every planned net appears on both faces: its pogo pad and its land.
    for board in [0, 1] {
        let net = format!("\"B{board}.TP_DP.TP\"");
        assert_eq!(text.matches(net.as_str()).count(), 2, "pogo + land");
    }
    assert_eq!(text.matches("Interposer:806-22-001-30-0xx191").count(), 12);

    // Deterministic output.
    assert_eq!(
        text,
        pcb_interposer::emit::board(&panel, &lands, Bed::S13, Some(&plan), &[], Layers::Two)
    );

    // Without a plan the board stays bare: no pogos, no planned nets.
    let bare = pcb_interposer::emit::board(&panel, &lands, Bed::S13, None, &[], Layers::Two);
    assert!(!bare.contains("Pogo_Pad"));
    assert!(!bare.contains("B0.TP_DP.TP"));
}

#[test]
fn four_layers_add_inner_gnd_planes() {
    let ipc = ipc2581::Ipc2581::parse(PANEL).expect("panel fixture parses");
    let panel = pcb_interposer::panel::extract(&ipc).expect("panel extracts");
    let lands = pcb_interposer::pattern::oriented_s13(panel.width, panel.height);
    let four = pcb_interposer::emit::board(&panel, &lands, Bed::S13, None, &[], Layers::Four);
    pcb_sexpr::parse(&four).expect("board parses");
    // One GND pour per copper layer, on the default 4-layer stackup.
    for layer in ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"] {
        assert!(four.contains(&format!("(net \"GND\")\n\t\t(layer \"{layer}\")")));
    }
    assert!(four.contains("(thickness 1.6062)"));
}

#[test]
fn four_signal_layers_route_on_in2() {
    let ipc = ipc2581::Ipc2581::parse(PANEL).expect("panel fixture parses");
    let panel = pcb_interposer::panel::extract(&ipc).expect("panel extracts");
    let lands = pcb_interposer::pattern::oriented_s13(panel.width, panel.height);
    let four = pcb_interposer::emit::board(&panel, &lands, Bed::S13, None, &[], Layers::FourSignal);
    pcb_sexpr::parse(&four).expect("board parses");
    // GND under the top face; In2 a pour-free signal layer.
    for layer in ["F.Cu", "In1.Cu", "B.Cu"] {
        assert!(four.contains(&format!("(net \"GND\")\n\t\t(layer \"{layer}\")")));
    }
    assert!(!four.contains("(net \"GND\")\n\t\t(layer \"In2.Cu\")"));
    assert!(four.contains("\"In1.Cu\" power"));
    assert!(four.contains("\"In2.Cu\" signal"));
    assert_eq!("4-signal".parse::<Layers>(), Ok(Layers::FourSignal));
}

#[test]
fn plans_the_fixture_map() {
    let ipc = ipc2581::Ipc2581::parse(PANEL).expect("panel fixture parses");
    let panel = pcb_interposer::panel::extract(&ipc).expect("panel extracts");
    let lands = pcb_interposer::pattern::oriented_s13(panel.width, panel.height);
    let contacts =
        pcb_interposer::contacts::extract_contacts(&ipc, panel.height).expect("contacts");
    // 2 board instances × 6 marked components.
    assert_eq!(contacts.len(), 12);
    // Board 1 sits one grid pitch below board 0 in the Y-down frame.
    let tp1 = |board: u32| {
        contacts
            .iter()
            .find(|c| c.board == board && c.refdes == "TP1")
            .unwrap()
    };
    assert_eq!(tp1(0).xy[0], tp1(1).xy[0]);
    assert!((tp1(0).xy[1] - tp1(1).xy[1] - 40.0).abs() < 1e-9);

    let plan = pcb_interposer::plan::plan(contacts, &lands, &[], Bed::S13).expect("plan");
    assert_eq!(plan.boards_total, 2);
    assert_eq!(plan.tested, vec![0, 1]);
    // 12 contacts bound: 10 to lands, 2 gnd to the pour.
    assert_eq!(plan.bindings.len(), 12);
    assert_eq!(plan.bindings.iter().filter(|b| b.land.is_none()).count(), 2);
    for binding in &plan.bindings {
        let contact = &plan.contacts[binding.contact];
        match binding.land {
            None => assert_eq!(binding.net, "GND"),
            Some(land) => {
                // Role-faithful: a contact lands on its own role's land
                // (low-speed collapses onto Ls).
                let land_role = lands[land].role;
                assert_eq!(land_role, contact.role);
                assert_eq!(
                    binding.net,
                    format!(
                        "B{}.{}.TP",
                        contact.board,
                        contact.path.trim_end_matches(".TP")
                    )
                );
            }
        }
    }
    // The USB pair of one board shares a kit block.
    let block_of = |refdes: &str| {
        let binding = plan
            .bindings
            .iter()
            .find(|b| {
                let c = &plan.contacts[b.contact];
                c.board == 0 && c.refdes == refdes
            })
            .unwrap();
        lands[binding.land.unwrap()].block
    };
    assert_eq!(block_of("TP1"), block_of("TP2"));

    // Net classes bind by exact net name: VTarget wide, the USB pair USB.
    let project: serde_json::Value = serde_json::from_str(&pcb_interposer::emit::project(
        Bed::S13,
        Some(&plan),
        Layers::Two,
    ))
    .expect("project is json");
    let class_of = |net: &str| {
        project["net_settings"]["netclass_patterns"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["pattern"] == net)
            .map(|entry| entry["netclass"].as_str().unwrap().to_string())
    };
    assert_eq!(class_of("B0.TP_DP.TP").as_deref(), Some("USB"));
    assert_eq!(class_of("B1.TP_VT.TP").as_deref(), Some("Power"));
    assert_eq!(class_of("B0.TP_SWDIO.TP"), None);

    // The JSON document is deterministic.
    let json = pcb_interposer::plan::to_json(&plan, &panel, &lands);
    let value: serde_json::Value = serde_json::from_str(&json).expect("valid json");
    assert_eq!(value["boards_total"], 2);
    assert_eq!(value["bindings"].as_array().unwrap().len(), 12);
}

#[test]
fn refuses_non_standard_panel_sizes() {
    let custom = PANEL.replace("74.0", "94.0").replace("105.0", "82.0");
    let ipc = ipc2581::Ipc2581::parse(&custom).expect("fixture parses");
    let err = pcb_interposer::panel::extract(&ipc).unwrap_err();
    assert!(err.to_string().contains("unsupported panel size"), "{err}");
}

#[test]
fn arc_midpoints_stay_on_the_radius() {
    let ipc = ipc2581::Ipc2581::parse(PANEL).expect("panel fixture parses");
    let panel = pcb_interposer::panel::extract(&ipc).expect("panel extracts");
    for outline in &panel.outline {
        if let pcb_interposer::panel::Outline::Arc { start, mid, end } = outline {
            // All fixture arcs are 3 mm corner rounds; the mid point must
            // sit on the same circle as the endpoints.
            let chord = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
            assert!((chord - 3.0 * std::f64::consts::SQRT_2).abs() < 1e-9);
            let to_mid = ((mid[0] - start[0]).powi(2) + (mid[1] - start[1]).powi(2)).sqrt();
            // Start-to-mid spans half the 90° sweep.
            assert!((to_mid - 2.0 * 3.0 * (std::f64::consts::PI / 8.0).sin()).abs() < 1e-9);
        }
    }
}

#[test]
fn switchyard_rev1_carries_its_alignment() {
    let panel = pcb_interposer::panel::Panel {
        width: 105.0,
        height: 74.0,
        outline: Vec::new(),
        holes: Vec::new(),
        fids_top: Vec::new(),
        fids_bottom: Vec::new(),
        mouse_bites: Vec::new(),
    };
    let bed = Bed::SwitchyardRev1;
    let panel = bed.fit(panel);
    // Cut to the conveyor envelope, with its own three fiducials.
    assert_eq!(panel.outline.len(), 4);
    assert_eq!(panel.fids_top.len(), 3);
    let lands = bed.lands(panel.width, panel.height).expect("rev-1 lands");
    let straps = bed.id_straps(&lands, Some(433)).expect("straps");
    let text = pcb_interposer::emit::board(&panel, &lands, bed, None, &straps, Layers::Two);
    // ID 433 (digits 1 2 3 0 1, check 1) straps ID1, ID5 and ID6 to ID7,
    // ID2 to ID8 and ID4 to GND, and leaves ID3 open.
    // Each strapped land, a stub from it to its net's lane, and the lane:
    // ID7 just above the ID row (y 67), ID8 just below it, both locked.
    assert_eq!(text.matches("(net \"ID7\")").count(), 4 + 4 + 1);
    assert_eq!(text.matches("(net \"ID8\")").count(), 2 + 2 + 1);
    assert!(text.contains("(start 16 65.625)\n\t\t(end 80.5 65.625)"));
    assert!(text.contains("(start 26.5 68.375)\n\t\t(end 83.5 68.375)"));
    assert_eq!(text.matches("(locked yes)").count(), 2 + 4 + 1 + 2 + 1);
    pcb_sexpr::parse(&text).expect("board parses");
    // D1 and the M3 are round, D2 a slot along x; each keeps copper
    // outside its Ø6 keep-out.
    assert_eq!(text.matches("Interposer:Alignment_").count(), 3);
    assert!(text.contains("(drill 3.05)"));
    assert!(text.contains("(drill oval 4 3.05)"));
    assert!(text.contains("(drill 3.4)"));
    assert!(text.contains("(clearance 1.475)"));
    assert!(text.contains("(clearance 1.3)"));
    assert_eq!(text.matches("Interposer:Mate_Pad_D2mm").count(), 128);
    assert!(text.contains("(start 5 5)\n\t\t(end 100 5)"));

    // VTarget and the signal lanes may carry 60 V: their classes keep the
    // coated 0.13 mm, and the custom rules 0.6 mm at bare pads.
    let project: serde_json::Value =
        serde_json::from_str(&pcb_interposer::emit::project(bed, None, Layers::Two))
            .expect("project is json");
    let classes = project["net_settings"]["classes"].as_array().unwrap();
    for name in ["HV", "Power"] {
        let class = classes.iter().find(|c| c["name"] == name).unwrap();
        assert_eq!(class["clearance"], 0.13, "{name}");
    }
    let usb = classes.iter().find(|c| c["name"] == "USB").unwrap();
    assert_eq!(usb["clearance"], 0.1);
    let rules = pcb_interposer::emit::rules(bed, Layers::Two).expect("rev-1 has high voltage");
    assert_eq!(rules.matches("(min 0.6mm)").count(), 2);
    assert!(rules.contains("B.hasNetclass('HV') || B.hasNetclass('Power')"));
    assert!(pcb_interposer::emit::rules(Bed::S13, Layers::Two).is_none());

    // Over two inner GND planes every class keeps the exposed 0.6 mm, so
    // no custom rules; the strap lanes no longer fit above the edge and
    // the straps are left to the router.
    assert_eq!(Layers::Four.uniform_clearance(bed), Some(0.6));
    assert_eq!(Layers::Four.uniform_clearance(Bed::S13), None);
    assert_eq!(Layers::FourSignal.uniform_clearance(bed), None);
    let project: serde_json::Value =
        serde_json::from_str(&pcb_interposer::emit::project(bed, None, Layers::Four))
            .expect("project is json");
    for class in project["net_settings"]["classes"].as_array().unwrap() {
        assert_eq!(class["clearance"], 0.6, "{}", class["name"]);
    }
    assert!(pcb_interposer::emit::rules(bed, Layers::Four).is_none());
    let four = pcb_interposer::emit::board(&panel, &lands, bed, None, &straps, Layers::Four);
    pcb_sexpr::parse(&four).expect("board parses");
    assert_eq!(four.matches("(net \"ID7\")").count(), 4);
    assert_eq!(four.matches("(locked yes)").count(), 2);
}
