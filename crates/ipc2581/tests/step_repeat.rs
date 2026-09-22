use ipc2581::{Ipc2581, StepType};

#[test]
fn parses_step_repeat_on_step() {
    let doc = Ipc2581::parse(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="panel"/>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Step name="board" type="BOARD"/>
      <Step name="panel" type="PALLET">
        <StepRepeat stepRef="board" x="7.5" y="9.25" nx="2" ny="3" dx="30" dy="20" angle="90" mirror="true"/>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
    )
    .unwrap();
    let panel = &doc.ecad().unwrap().cad_data.steps[1];

    assert_eq!(panel.step_type, Some(StepType::Pallet));
    let [repeat] = &panel.step_repeats[..] else {
        panic!("expected one StepRepeat");
    };
    assert_eq!(doc.resolve(repeat.step_ref), "board");
    assert_eq!(
        (repeat.x, repeat.y, repeat.dx, repeat.dy),
        (7.5, 9.25, 30.0, 20.0)
    );
    assert_eq!((repeat.nx, repeat.ny), (2, 3));
    assert_eq!(repeat.angle, 90.0);
    assert!(repeat.mirror);
}
