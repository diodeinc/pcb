use gerberx2::{
    ApertureTemplate, AttributeValue, Command, Contour, ContourSegment, GerberLayer, GerberX2,
    ObjectKind, PathCommand, Point, Polarity, Unit, WriterAperture, WriterApertureMacro,
    WriterApertureTemplate, WriterMacroExpression, WriterMacroPrimitive, WriterObject,
};

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
fn writes_idiomatic_x2_layer_from_object_ir() {
    let mut layer = GerberLayer {
        file_attributes: vec![
            AttributeValue::new(".FileFunction", ["Copper", "L1", "Top"]),
            AttributeValue::new(".Part", ["Single"]),
        ],
        apertures: vec![
            WriterAperture {
                code: 10,
                template: WriterApertureTemplate::Circle {
                    diameter: 0.2,
                    hole_diameter: None,
                },
                attributes: vec![AttributeValue::new(".AperFunction", ["Conductor"])],
            },
            WriterAperture {
                code: 11,
                template: WriterApertureTemplate::Rectangle {
                    width: 1.0,
                    height: 1.5,
                    hole_diameter: None,
                },
                attributes: vec![AttributeValue::new(".AperFunction", ["SMDPad", "CuDef"])],
            },
        ],
        ..GerberLayer::default()
    };
    layer.objects = vec![
        WriterObject {
            kind: ObjectKind::Flash {
                at: Point { x: 1.0, y: 2.0 },
                aperture: 11,
            },
            polarity: Polarity::Dark,
            attributes: vec![
                AttributeValue::new(".N", ["GND"]),
                AttributeValue::new(".C", ["U1"]),
                AttributeValue::new(".P", ["1"]),
            ],
        },
        WriterObject::dark(ObjectKind::Draw {
            start: Point { x: 1.0, y: 2.0 },
            end: Point { x: 3.0, y: 2.0 },
            aperture: 10,
        }),
        WriterObject::dark(ObjectKind::Arc {
            start: Point { x: 3.0, y: 2.0 },
            end: Point { x: 4.0, y: 3.0 },
            center_offset: Point { x: 0.5, y: 0.5 },
            clockwise: false,
            aperture: 10,
        }),
        WriterObject::dark(ObjectKind::Region {
            contours: vec![Contour {
                segments: vec![
                    ContourSegment::Line {
                        start: Point { x: 0.0, y: 0.0 },
                        end: Point { x: 1.0, y: 0.0 },
                    },
                    ContourSegment::Line {
                        start: Point { x: 1.0, y: 0.0 },
                        end: Point { x: 1.0, y: 1.0 },
                    },
                    ContourSegment::Line {
                        start: Point { x: 1.0, y: 1.0 },
                        end: Point { x: 0.0, y: 0.0 },
                    },
                ],
            }],
        }),
    ];

    let output = gerberx2::write_layer(&layer).unwrap();
    assert!(output.contains("%TF.FileFunction,Copper,L1,Top*%"));
    assert!(output.contains("%TA.AperFunction,SMDPad,CuDef*%"));
    assert!(output.contains("%TO.N,GND*%"));
    assert!(output.contains("G36*"));

    let parsed = GerberX2::parse(&output).unwrap();
    assert_eq!(parsed.file_attributes().len(), 2);
    assert_eq!(parsed.aperture_definitions().len(), 2);
    assert_eq!(parsed.objects().len(), 4);
    assert!(matches!(
        parsed.objects()[0].kind,
        ObjectKind::Flash { at, aperture: 11 } if at.x == 1.0 && at.y == 2.0
    ));
    assert!(matches!(
        parsed.objects()[2].kind,
        ObjectKind::Arc {
            clockwise: false,
            ..
        }
    ));
    assert_eq!(parsed.objects()[0].object_attributes.len(), 3);
}

#[test]
fn writes_macro_and_block_apertures_without_flattening() {
    let layer = GerberLayer {
        aperture_macros: vec![WriterApertureMacro {
            name: "ROUNDRECT".to_string(),
            primitives: vec![
                WriterMacroPrimitive::Comment("rounded rectangle test macro".to_string()),
                WriterMacroPrimitive::VariableDefinition {
                    variable: 3,
                    expression: WriterMacroExpression::Add(
                        Box::new(WriterMacroExpression::Variable(1)),
                        Box::new(WriterMacroExpression::Multiply(
                            Box::new(WriterMacroExpression::Variable(2)),
                            Box::new(WriterMacroExpression::Number(2.0)),
                        )),
                    ),
                },
                WriterMacroPrimitive::Shape {
                    code: 1,
                    parameters: vec![
                        WriterMacroExpression::Number(1.0),
                        WriterMacroExpression::Variable(3),
                        WriterMacroExpression::Number(0.0),
                        WriterMacroExpression::Number(0.0),
                        WriterMacroExpression::Number(0.0),
                    ],
                },
            ],
        }],
        apertures: vec![
            WriterAperture {
                code: 10,
                template: WriterApertureTemplate::Circle {
                    diameter: 0.1,
                    hole_diameter: None,
                },
                attributes: Vec::new(),
            },
            WriterAperture {
                code: 11,
                template: WriterApertureTemplate::Macro {
                    name: "ROUNDRECT".to_string(),
                    parameters: vec![0.2, 0.4],
                },
                attributes: vec![AttributeValue::new(".AperFunction", ["SMDPad", "CuDef"])],
            },
            WriterAperture {
                code: 20,
                template: WriterApertureTemplate::Block {
                    objects: vec![WriterObject::dark(ObjectKind::Flash {
                        at: Point { x: 1.0, y: 0.0 },
                        aperture: 10,
                    })],
                },
                attributes: Vec::new(),
            },
        ],
        objects: vec![
            WriterObject::dark(ObjectKind::Flash {
                at: Point { x: 0.0, y: 0.0 },
                aperture: 11,
            }),
            WriterObject::dark(ObjectKind::Flash {
                at: Point { x: 2.0, y: 3.0 },
                aperture: 20,
            }),
        ],
        ..GerberLayer::default()
    };

    let output = gerberx2::write_layer(&layer).unwrap();
    assert!(output.contains("%AMROUNDRECT*"));
    assert!(output.contains("%ADD11ROUNDRECT,0.2X0.4*%"));
    assert!(output.contains("%ABD20*%"));

    let parsed = GerberX2::parse(&output).unwrap();
    assert_eq!(parsed.aperture_macros().len(), 1);
    assert_eq!(parsed.aperture_definitions().len(), 3);
    assert!(matches!(
        parsed.aperture_definitions()[1].template,
        ApertureTemplate::Macro { .. }
    ));
    assert!(matches!(
        parsed.aperture_definitions()[2].template,
        ApertureTemplate::Block { .. }
    ));
    assert_eq!(parsed.objects().len(), 2);
    assert!(matches!(
        parsed.objects()[1].kind,
        ObjectKind::Flash { at, aperture: 10 } if at.x == 3.0 && at.y == 3.0
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

#[test]
fn lowers_aperture_macro_primitives_to_geometry_paths() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%AMMAC*\n0 comment*\n$3=$1+$2x2*\n1,1,$3,0,0,0*\n20,1,0.1,-0.5,0,0.5,0,0*\n21,0,0.2,0.3,0,0,0*\n4,1,3,0,0,1,0,0,1,0,0,0*\n5,1,6,0,0,1.2,30*\n7,0,0,1.0,0.5,0.1,45*\n%\n%ADD10MAC,0.2X0.4*%\nD10*\nX0Y0D03*\nM02*\n",
    )
    .unwrap();

    assert_eq!(gerber.aperture_macros().len(), 1);
    let geometry = gerber.aperture_definitions()[0].geometry.as_ref().unwrap();
    assert_eq!(geometry.paths.len(), 9);
    assert_eq!(geometry.paths[0].polarity, Polarity::Dark);
    assert_eq!(geometry.paths[2].polarity, Polarity::Clear);
    assert!(matches!(
        geometry.paths[3].contours[0].commands.last(),
        Some(PathCommand::Close)
    ));
}

#[test]
fn expands_block_apertures_when_flashed() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,0.1*%\n%ABD20*%\nD10*\nX1000000Y0D03*\n%AB*%\nD20*\nX2000000Y3000000D03*\nM02*\n",
    )
    .unwrap();

    assert_eq!(gerber.aperture_definitions().len(), 2);
    assert!(matches!(
        gerber.aperture_definitions()[1].template,
        ApertureTemplate::Block { .. }
    ));
    assert_eq!(gerber.objects().len(), 1);
    assert!(matches!(
        gerber.objects()[0].kind,
        ObjectKind::Flash {
            at,
            aperture: 10,
        } if at.x == 3.0 && at.y == 3.0
    ));
}

#[test]
fn expands_step_repeat_in_y_then_x_order() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,0.1*%\nD10*\n%SRX2Y2I1.0J2.0*%\nX0Y0D03*\n%SR*%\nM02*\n",
    )
    .unwrap();

    let points = gerber
        .objects()
        .iter()
        .map(|object| match object.kind {
            ObjectKind::Flash { at, .. } => (at.x, at.y),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    assert_eq!(points, vec![(0.0, 0.0), (0.0, 2.0), (1.0, 0.0), (1.0, 2.0)]);
}

#[test]
fn rejects_unclosed_region_contours() {
    let err = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\nG36*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nX1000000Y1000000D01*\nG37*\nM02*\n",
    )
    .unwrap_err();

    assert!(err.to_string().contains("region contour must be closed"));
}

#[test]
fn extracts_and_processes_render_geometry() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.2*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nX1000000Y1000000D03*\nM02*\n",
    )
    .unwrap();

    let mut geometry = gerberx2::geometry::extract_document(&gerber);
    assert_eq!(geometry.file_function, vec!["Copper", "L1", "Top"]);
    assert_eq!(geometry.features.len(), 2);
    assert!(geometry.paths.iter().any(|path| path.flags.stroked));

    gerberx2::geometry::process::process_document(&mut geometry);
    assert!(geometry.features.iter().any(|feature| {
        feature.kind == gerberx2::geometry::ir::FeatureKind::Composite && feature.path_count == 1
    }));
    assert!(!geometry.bbox.is_empty());
}

#[test]
fn process_applies_clear_polarity_cutouts() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10R,2.0X2.0*%\n%ADD11C,1.0*%\nD10*\nX0Y0D03*\n%LPC*%\nD11*\nX0Y0D03*\nM02*\n",
    )
    .unwrap();

    let mut geometry = gerberx2::geometry::extract_document(&gerber);
    gerberx2::geometry::process::process_document(&mut geometry);
    let composite = geometry
        .features
        .iter()
        .find(|feature| feature.kind == gerberx2::geometry::ir::FeatureKind::Composite)
        .unwrap();
    let path = &geometry.paths[composite.path_start as usize];
    assert!(path.contour_count >= 2);
}

#[test]
fn extracts_non_circular_aperture_sweeps_without_diagnostics() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10R,0.2X0.4*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nM02*\n",
    )
    .unwrap();

    let geometry = gerberx2::geometry::extract_document(&gerber);
    assert!(geometry.diagnostics.is_empty());
    assert!(geometry.paths.len() > 1);
}

#[test]
fn renders_svg_and_png_from_processed_geometry() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%TF.FileFunction,Paste,Top*%\n%ADD10R,1.0X1.0*%\nD10*\nX0Y0D03*\nM02*\n",
    )
    .unwrap();

    let mut geometry = gerberx2::geometry::extract_document(&gerber);
    gerberx2::geometry::process::process_document(&mut geometry);
    let svg = gerberx2::geometry::svg::render_svg(&geometry);
    assert!(svg.contains("<svg"));
    assert!(svg.contains("<path"));
    assert!(svg.contains("Paste, Top"));
    let png = gerberx2::geometry::raster::render_png_with_max_dimension(&geometry, 64).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
}

#[test]
fn renders_profile_gerber_as_black_board_outline() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%TF.FileFunction,Profile,NP*%\n%ADD10C,0.1*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nX1000000Y1000000D01*\nX0Y1000000D01*\nX0Y0D01*\nM02*\n",
    )
    .unwrap();

    let mut geometry = gerberx2::geometry::extract_document(&gerber);
    gerberx2::geometry::process::process_document(&mut geometry);
    let svg = gerberx2::geometry::svg::render_svg(&geometry);

    assert!(svg.contains("fill='none' stroke='#000000'"));
    assert!(svg.contains("data-board-outline='true'"));
    assert!(!svg.contains("fill='#606060'"));
}
