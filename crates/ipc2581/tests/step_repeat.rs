use ipc2581::Ipc2581;

fn panel_fixture() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="5"/>
            <PolyStepSegment x="0" y="5"/>
          </Polygon>
        </Profile>
      </Step>
      <Step name="panel" type="PALLET">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="100" y="0"/>
            <PolyStepSegment x="100" y="80"/>
            <PolyStepSegment x="0" y="80"/>
          </Polygon>
        </Profile>
        <StepRepeat stepRef="board" x="7.5" y="9.25" nx="2" ny="3" dx="30" dy="20" angle="90" mirror="true"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#
}

#[test]
fn parses_step_repeat_on_step() {
    let doc = Ipc2581::parse(panel_fixture()).expect("synthetic fixture should parse");
    let ecad = doc.ecad().expect("fixture has ECAD data");
    let panel = ecad
        .cad_data
        .steps
        .iter()
        .find(|step| doc.resolve(step.name) == "panel")
        .expect("fixture has panel step");

    assert_eq!(panel.step_repeats.len(), 1);
    let repeat = &panel.step_repeats[0];
    assert_eq!(doc.resolve(repeat.step_ref), "board");
    assert_eq!(repeat.x, 7.5);
    assert_eq!(repeat.y, 9.25);
    assert_eq!(repeat.nx, 2);
    assert_eq!(repeat.ny, 3);
    assert_eq!(repeat.dx, 30.0);
    assert_eq!(repeat.dy, 20.0);
    assert_eq!(repeat.angle, 90.0);
    assert!(repeat.mirror);
}
