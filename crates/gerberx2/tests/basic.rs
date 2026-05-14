use gerberx2::{ApertureTemplate, Command, GerberX2, ObjectKind, PathCommand, Unit};

#[test]
fn parses_basic_x2_layer() {
    let gerber = GerberX2::parse(
        "G04 paste layer*\n%FSLAX36Y36*%\n%MOMM*%\n%TF.FileFunction,Paste,Top*%\n%TA.AperFunction,Material*%\n%ADD10R,0.93X0.93*%\nD10*\nX142000000Y-108550000D03*\nM02*\n",
    )
    .unwrap();

    assert_eq!(gerber.final_state().unit, Some(Unit::Millimeter));
    assert_eq!(gerber.file_attributes().len(), 1);
    assert_eq!(gerber.aperture_definitions().len(), 1);
    assert!(matches!(
        gerber.aperture_definitions()[0].template,
        ApertureTemplate::Rectangle {
            width: 0.93,
            height: 0.93,
            hole_diameter: None
        }
    ));
    assert!(
        gerber
            .commands()
            .iter()
            .any(|command| matches!(command, Command::Operation { .. }))
    );
    assert_eq!(gerber.objects().len(), 1);
    assert!(matches!(
        gerber.objects()[0].kind,
        ObjectKind::Flash {
            at,
            aperture: 10,
        } if at.x == 142.0 && at.y == -108.55
    ));
}

#[test]
fn builds_draw_arc_and_region_objects() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%TA.AperFunction,Conductor*%\n%ADD10C,0.2*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nG75*\nG02*\nX1000000Y1000000I0J500000D01*\nG36*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nX1000000Y1000000D01*\nX0Y0D01*\nG37*\nM02*\n",
    )
    .unwrap();

    assert_eq!(gerber.objects().len(), 3);
    assert!(matches!(
        gerber.objects()[0].kind,
        ObjectKind::Draw {
            start,
            end,
            aperture: 10,
        } if start.x == 0.0 && start.y == 0.0 && end.x == 1.0 && end.y == 0.0
    ));
    assert!(matches!(
        gerber.objects()[1].kind,
        ObjectKind::Arc {
            end,
            center_offset,
            clockwise: true,
            aperture: 10,
            ..
        } if end.x == 1.0 && end.y == 1.0 && center_offset.x == 0.0 && center_offset.y == 0.5
    ));
    assert!(matches!(
        &gerber.objects()[2].kind,
        ObjectKind::Region { contours } if contours.len() == 1 && contours[0].segments.len() == 3
    ));
}

#[test]
fn lowers_standard_apertures_to_geometry_paths() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,1.0X0.25*%\n%ADD11R,1.0X2.0*%\n%ADD12O,2.0X1.0*%\n%ADD13P,2.0X6X30*%\nD10*\nX0Y0D03*\nM02*\n",
    )
    .unwrap();

    assert_eq!(gerber.aperture_definitions().len(), 4);
    let circle = gerber.aperture_definitions()[0].geometry.as_ref().unwrap();
    assert_eq!(circle.paths.len(), 2);
    assert!(matches!(
        circle.paths[0].contours[0].commands[1],
        PathCommand::ArcTo { .. }
    ));
    let rect = gerber.aperture_definitions()[1].geometry.as_ref().unwrap();
    assert_eq!(rect.paths[0].contours[0].commands.len(), 5);
    let obround = gerber.aperture_definitions()[2].geometry.as_ref().unwrap();
    assert_eq!(obround.paths[0].contours[0].commands.len(), 6);
    let polygon = gerber.aperture_definitions()[3].geometry.as_ref().unwrap();
    assert_eq!(polygon.paths[0].contours[0].commands.len(), 7);
}
