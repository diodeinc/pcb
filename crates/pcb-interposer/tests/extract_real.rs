//! Drive shipped extract on a checked-in tagged DUT snippet.

use pcb_interposer::extract::{extract_ipc_xml, extract_kicad_src, is_v1_testpoint};
use pcb_interposer::types::{BoardId, Ict};
use std::collections::BTreeMap;

#[test]
fn real_fixture_finds_ict_testpoints_skips_tagconnect() {
    let src = include_str!("fixtures/tagged_tps.kicad_pcb");
    let cs = extract_kicad_src(src, BoardId(0), &BTreeMap::new()).unwrap();
    assert_eq!(cs.len(), 2);
    assert!(cs.iter().all(|c| is_v1_testpoint(&c.package)));
    assert!(cs.iter().all(|c| c.side == "bottom"));
    assert!(
        cs.iter()
            .any(|c| c.ict == Ict::Gnd && c.path.contains("TP_GND"))
    );
    assert!(cs.iter().any(|c| c.ict == Ict::Vtarget));
    assert!(cs.iter().all(|c| !c.package.contains("TC2030")));
    assert!(
        cs.iter().all(|c| !c.path.contains("FRONT")),
        "F.Cu+Ict TestPoint must be skipped"
    );
}

#[test]
fn ipc_path_keyed_skips_non_1mm() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
  <Bom name="B">
    <BomHeader assembly="A" revision="1"/>
    <BomItem OEMDesignNumberRef="TP_GND.TP" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP1" packageRef="TestPoint_Pad_D1.0mm" populate="true" layerRef="B.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="KICAD" textualCharacteristicName="Ict" textualCharacteristicValue="gnd"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="J_SWD.J" quantity="1" pinCount="6" category="ELECTRICAL">
      <RefDes name="J1" packageRef="TC2030-NL_SWD" populate="true" layerRef="B.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="KICAD" textualCharacteristicName="Ict" textualCharacteristicValue="swd"/>
      </Characteristics>
    </BomItem>
  </Bom>
</IPC-2581>"#;
    let cs = extract_ipc_xml(xml, BoardId(0)).unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].ict, Ict::Gnd);
    assert!(is_v1_testpoint(&cs[0].package));
}

#[test]
fn ipc_front_side_ict_is_skipped() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner">
    <FunctionMode mode="ASSEMBLY"/>
    <DictionaryColor/>
    <DictionaryLineDesc units="MILLIMETER"/>
    <DictionaryFillDesc units="MILLIMETER"/>
    <DictionaryStandard units="MILLIMETER"/>
    <DictionaryUser units="MILLIMETER"/>
  </Content>
  <Bom name="B">
    <BomHeader assembly="A" revision="1"/>
    <BomItem OEMDesignNumberRef="TP_FRONT.TP" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP1" packageRef="TestPoint_Pad_D1.0mm" populate="true" layerRef="F.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="KICAD" textualCharacteristicName="Ict" textualCharacteristicValue="gnd"/>
      </Characteristics>
    </BomItem>
    <BomItem OEMDesignNumberRef="TP_BACK.TP" quantity="1" pinCount="1" category="ELECTRICAL">
      <RefDes name="TP2" packageRef="TestPoint_Pad_D1.0mm" populate="true" layerRef="B.Cu"/>
      <Characteristics category="ELECTRICAL">
        <Textual definitionSource="KICAD" textualCharacteristicName="Ict" textualCharacteristicValue="ls"/>
      </Characteristics>
    </BomItem>
  </Bom>
</IPC-2581>"#;
    let cs = extract_ipc_xml(xml, BoardId(0)).unwrap();
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].ict, Ict::Ls);
    assert_eq!(cs[0].path, "TP_BACK.TP");
}
