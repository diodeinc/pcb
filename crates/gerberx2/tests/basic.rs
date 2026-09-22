use gerberx2::geometry::GerberArtworkDocument;
use gerberx2::{
    ApertureTemplate, AttributeSets, AttributeValue, Contour, ContourSegment, GerberLayer,
    GerberX2, ObjectKind, PathCommand, Point, StepRepeat, WriterAperture, WriterApertureTemplate,
    WriterObject,
};
use pcb_ir::dialects::artwork::{Geometry, Object};
use pcb_ir::geom::{GeometryAccuracy, Polarity, Resolution};

#[test]
fn parses_basic_x2_layer() {
    let gerber = GerberX2::parse(
        "G04 paste layer*\n%FSLAX36Y36*%\n%MOMM*%\n%TF.FileFunction,Paste,Top*%\n%TA.AperFunction,Material*%\n%ADD10R,0.93X0.93*%\nD10*\nX142000000Y-108550000D03*\nM02*\n",
    )
    .unwrap();

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
    let mut attribute_sets = AttributeSets::default();
    let conductor =
        attribute_sets.intern(vec![AttributeValue::new(".AperFunction", ["Conductor"])]);
    let smd_pad = attribute_sets.intern(vec![AttributeValue::new(
        ".AperFunction",
        ["SMDPad", "CuDef"],
    )]);
    let pin = attribute_sets.intern(vec![
        AttributeValue::new(".N", ["GND"]),
        AttributeValue::new(".C", ["U1"]),
        AttributeValue::new(".P", ["U1", "1"]),
    ]);
    let mut layer = GerberLayer {
        file_attributes: vec![
            AttributeValue::new(".FileFunction", ["Copper", "L1", "Top"]),
            AttributeValue::new(".Part", ["Single"]),
            AttributeValue::new(".SameCoordinates", std::iter::empty::<&str>()),
        ],
        attribute_sets,
        apertures: vec![
            WriterAperture {
                code: 10,
                template: WriterApertureTemplate::Circle {
                    diameter: 0.2,
                    hole_diameter: None,
                },
                attributes: conductor,
            },
            WriterAperture {
                code: 11,
                template: WriterApertureTemplate::Rectangle {
                    width: 1.0,
                    height: 1.5,
                    hole_diameter: None,
                },
                attributes: smd_pad,
            },
        ],
        ..GerberLayer::default()
    };
    layer.objects = vec![
        WriterObject::new(
            ObjectKind::Flash {
                at: Point { x: 1.0, y: 2.0 },
                aperture: 11,
            },
            Polarity::Dark,
            pin,
        ),
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
        WriterObject {
            aperture_attributes: conductor,
            ..WriterObject::dark(ObjectKind::Region {
                contours: vec![Contour {
                    segments: [
                        (0.0, 0.0, 1.0, 0.0),
                        (1.0, 0.0, 1.0, 1.0),
                        (1.0, 1.0, 0.0, 0.0),
                    ]
                    .map(|(x0, y0, x1, y1)| ContourSegment::Line {
                        start: Point { x: x0, y: y0 },
                        end: Point { x: x1, y: y1 },
                    })
                    .to_vec(),
                }],
            })
        },
    ];

    let output = written(&layer);
    assert!(output.contains("%TF.FileFunction,Copper,L1,Top*%"));
    assert!(output.contains("%TF.SameCoordinates*%"));
    assert!(output.contains("%TA.AperFunction,SMDPad,CuDef*%"));
    assert!(output.contains("%TO.N,GND*%"));
    assert!(output.contains("%TA.AperFunction,Conductor*%\nG36*"));

    let parsed = GerberX2::parse(&output).unwrap();
    assert_eq!(parsed.file_attributes().len(), 3);
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
    assert!(
        parsed
            .attributes(parsed.objects()[3].aperture_attributes)
            .iter()
            .any(|attribute| parsed.resolve(attribute.name) == ".AperFunction")
    );
}

#[test]
fn objects_share_attribute_sets_until_the_dictionary_changes() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%%MOMM*%%ADD10C,1*%D10*%TO.N,A*%%TO.C,R1*%X0Y0D03*X1D03*%TD.C*%X2D03*%TO.N,B*%X3D03*%TD*%X4D03*M02*",
    )
    .unwrap();
    let sets = gerber
        .objects()
        .iter()
        .map(|object| {
            gerber
                .attributes(object.object_attributes)
                .iter()
                .map(|attribute| {
                    let fields = attribute.fields.iter().map(|field| gerber.resolve(*field));
                    format!(
                        "{}={}",
                        gerber.resolve(attribute.name),
                        fields.collect::<Vec<_>>().join(",")
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        sets,
        [
            vec![".N=A", ".C=R1"],
            vec![".N=A", ".C=R1"],
            vec![".N=A"],
            vec![".N=B"],
            vec![],
        ]
    );
    assert_eq!(
        gerber.objects()[0].object_attributes,
        gerber.objects()[1].object_attributes
    );
}

#[test]
fn coalesces_compatible_step_repeats() {
    let repeat = StepRepeat {
        x_repeats: 3,
        y_repeats: 2,
        x_step: 10.0,
        y_step: 20.0,
    };
    let repeated_flash = |x: f64, y: f64, attributes: u32| WriterObject {
        repeat: Some(repeat),
        attributes,
        ..flash(x, y)
    };
    let mut attribute_sets = AttributeSets::default();
    let ground = attribute_sets.intern(vec![AttributeValue::new(".N", ["GND"])]);
    let layer = GerberLayer {
        attribute_sets,
        ..circle_layer(
            1.0,
            vec![
                repeated_flash(2.0, 3.0, AttributeSets::EMPTY),
                repeated_flash(2.0, 4.0, ground),
                flash(2.0, 4.0),
            ],
        )
    };

    let output = written(&layer);
    assert_eq!(output.matches("%SRX3Y2I10J20*%").count(), 1);
    assert_eq!(output.matches("%SR*%").count(), 1);
    assert!(output.contains(
        "X2000000Y3000000D03*\n%TO.N,GND*%\nY4000000D03*\n%SR*%\n%TD*%\nX2000000Y4000000D03*"
    ));
    // The parsed stream holds the run once; imaging it repeats it.
    let parsed = GerberX2::parse(&output).unwrap();
    assert_eq!(parsed.objects().len(), 3);
    assert_eq!(parsed.step_repeats().len(), 1);
    assert_eq!(imaged(&parsed).len(), 13);
}

#[test]
fn preserves_polarity_order_across_step_repeats() {
    let repeat = StepRepeat {
        x_repeats: 2,
        y_repeats: 1,
        x_step: 10.0,
        y_step: 0.0,
    };
    let repeated_flash = |x: f64, polarity: Polarity| WriterObject {
        polarity,
        repeat: Some(repeat),
        ..flash(x, 0.0)
    };
    let layer = circle_layer(
        4.0,
        vec![
            repeated_flash(0.0, Polarity::Dark),
            repeated_flash(8.0, Polarity::Clear),
        ],
    );

    let output = written(&layer);
    assert_eq!(output.matches("%SRX2Y1I10J0*%").count(), 2);

    let objects = imaged(&GerberX2::parse(&output).unwrap());
    assert_eq!(objects.len(), 4);
    assert_eq!(
        objects
            .iter()
            .map(|object| object.polarity)
            .collect::<Vec<_>>(),
        [
            Polarity::Dark,
            Polarity::Dark,
            Polarity::Clear,
            Polarity::Clear,
        ]
    );
}

#[test]
fn writes_modal_coordinates_with_explicit_operations() {
    let layer = circle_layer(
        1.0,
        vec![
            flash(1.0, 2.0),
            flash(1.0, 3.0),
            flash(4.0, 3.0),
            flash(4.0, 3.0),
        ],
    );

    let output = written(&layer);
    assert!(output.contains("X1000000Y2000000D03*\nY3000000D03*\nX4000000D03*\nX4000000D03*"));

    let objects = GerberX2::parse(&output).unwrap().objects().to_vec();
    assert_eq!(objects.len(), 4);
    assert!(matches!(objects[1].kind, ObjectKind::Flash { at, .. } if at.x == 1.0 && at.y == 3.0));
    assert!(matches!(objects[2].kind, ObjectKind::Flash { at, .. } if at.x == 4.0 && at.y == 3.0));
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
fn normalizes_inch_coordinates_and_apertures_to_mm() {
    let gerber =
        GerberX2::parse("%FSLAX26Y26*%\n%MOIN*%\n%ADD10C,0.1X0.02*%\nD10*\nX1000000Y0D03*\nM02*\n")
            .unwrap();

    assert!(matches!(
        gerber.aperture_definitions()[0].template,
        ApertureTemplate::Circle {
            diameter,
            hole_diameter: Some(hole),
        } if close(diameter, 2.54) && close(hole, 0.508)
    ));
    assert!(matches!(
        gerber.objects()[0].kind,
        ObjectKind::Flash { at, .. } if close(at.x, 25.4) && close(at.y, 0.0)
    ));

    let geometry = extract(&gerber);
    let object = &geometry.objects[0];
    assert!(close(object.bbox.min.x, 25.4 - 1.27));
    assert!(close(object.bbox.max.x, 25.4 + 1.27));

    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOIN*%\n%AMMAC*\n1,1,$1,0,0,0*\n%\n%ADD10MAC,0.1*%\nD10*\nX0Y0D03*\nM02*\n",
    )
    .unwrap();
    let geometry = gerber.aperture_definitions()[0].geometry.as_ref().unwrap();
    assert!(matches!(
        geometry.paths[0].contours[0].commands[0],
        PathCommand::MoveTo(point) if close(point.x, 1.27) && close(point.y, 0.0)
    ));
}

#[test]
fn lowers_aperture_macro_primitives_to_geometry_paths() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%AMMAC*\n0 comment*\n$3=$1+$2x2*\n1,1,$3,0,0,0*\n20,1,0.1,-0.5,0,0.5,0,0*\n21,0,0.2,0.3,0,0,0*\n4,1,3,0,0,1,0,0,1,0,0,0*\n5,1,6,0,0,1.2,30*\n7,0,0,1.0,0.5,0.1,45*\n%\n%ADD10MAC,0.2X0.4*%\nD10*\nX0Y0D03*\nM02*\n",
    )
    .unwrap();

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
fn macro_primitives_rotate_about_the_macro_origin() {
    // Each primitive sits at (2, 0) and turns 90° about the macro origin, so
    // it must image around (0, 2) with its own axes turned as well.
    for (primitive, width, height) in [
        ("1,1,0.5,2,0,90", 0.5, 0.5),
        ("20,1,0.2,1.5,0,2.5,0,90", 0.2, 1.0),
        ("21,1,1.0,0.2,2,0,90", 0.2, 1.0),
        (
            "4,1,4,1.5,-0.1,2.5,-0.1,2.5,0.1,1.5,0.1,1.5,-0.1,90",
            0.2,
            1.0,
        ),
        ("5,1,4,2,0,1.0,90", 1.0, 1.0),
        ("7,2,0,1.0,0.5,0.1,90", 1.0, 1.0),
    ] {
        let gerber = GerberX2::parse(&format!(
            "%FSLAX26Y26*%\n%MOMM*%\n%AMMAC*\n{primitive}*\n%\n%ADD10MAC*%\nD10*\nX10000000Y20000000D03*\nM02*\n"
        ))
        .unwrap();
        let geometry = extract(&gerber);
        let bbox = geometry.objects[0].bbox;
        // The thermal's gaps clip its extreme points by a few microns.
        let near = |a: f64, b: f64| (a - b).abs() < 0.01;
        assert!(
            near(bbox.center().x, 10.0) && near(bbox.center().y, 22.0),
            "{primitive}: imaged around {:?}",
            bbox.center()
        );
        assert!(
            near(bbox.width(), width) && near(bbox.height(), height),
            "{primitive}: imaged {} x {}",
            bbox.width(),
            bbox.height()
        );
    }
}

#[test]
fn preserves_block_apertures_when_flashed() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,0.1*%\n%ABD20*%\nD10*\n%LPC*%\nX1000000Y0D03*\n%AB*%\nD20*\n%LPC*%\nX2000000Y3000000D03*\nM02*\n",
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
            aperture: 20,
        } if at.x == 2.0 && at.y == 3.0
    ));
    assert_eq!(gerber.objects()[0].polarity, Polarity::Clear);

    let artwork = extract(&gerber);
    assert_eq!(artwork.blocks.len(), 1);
    assert_eq!(artwork.blocks[0].objects.len(), 1);
    assert!(matches!(
        artwork.objects[0].geometry,
        Geometry::Instance { block: 0, .. }
    ));
    let expanded = pcb_ir::dialects::artwork::expand_instances(&artwork);
    assert_eq!(expanded.objects[0].polarity, Polarity::Dark);
}

#[test]
fn preserves_block_apertures_with_flash_transform() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,0.1*%\n%ABD20*%\nD10*\nX1000000Y0D03*\n%AB*%\n%LR90*%\n%LS2*%\nD20*\nX2000000Y3000000D03*\nM02*\n",
    )
    .unwrap();

    assert_eq!(gerber.objects().len(), 1);
    let object = &gerber.objects()[0];
    assert!(matches!(
        object.kind,
        ObjectKind::Flash {
            at,
            aperture: 20,
        } if close(at.x, 2.0) && close(at.y, 3.0)
    ));
    assert!(close(object.rotation_degrees, 90.0));
    assert!(close(object.scaling, 2.0));
    let artwork = extract(&gerber);
    assert!(matches!(
        artwork.objects[0].geometry,
        Geometry::Instance { block: 0, transform }
            if close(transform.m02, 2.0) && close(transform.m12, 3.0)
    ));
}

#[test]
fn images_step_repeat_in_y_then_x_order() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,0.1*%\nD10*\n%SRX2Y2I1.0J2.0*%\nX0Y0D03*\n%SR*%\nM02*\n",
    )
    .unwrap();

    let points = imaged(&gerber)
        .iter()
        .map(|object| match object.geometry {
            Geometry::Flash { transform, .. } => (transform.m02, transform.m12),
            _ => unreachable!(),
        })
        .collect::<Vec<_>>();
    assert_eq!(points, vec![(0.0, 0.0), (0.0, 2.0), (1.0, 0.0), (1.0, 2.0)]);
}

#[test]
fn step_repeats_stay_one_block_on_a_grid() {
    let accuracy = GeometryAccuracy::default();
    // A panel-sized repeat costs one seed run, and a lone occurrence or an
    // empty block costs nothing.
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%%MOMM*%%ADD10C,0.5*%D10*X0Y0D03*%SRX300Y200I1J1*%X0Y0D03*G01*X0Y0D02*X500000D01*%SR*%%SRX1Y1I0J0*%X0Y500000D03*%SR*%%SRX2Y2I1J1*%%SR*%M02*",
    )
    .unwrap();
    assert_eq!(gerber.objects().len(), 4);
    assert_eq!(gerber.step_repeats().len(), 1);

    let artwork = extract(&gerber);
    assert_eq!(artwork.blocks.len(), 1);
    assert_eq!(artwork.blocks[0].objects.len(), 2);
    assert_eq!(artwork.objects.len(), 3);
    assert!(matches!(
        artwork.objects[1].geometry,
        Geometry::GridInstance { block: 0, .. }
    ));
    assert!((artwork.layers[0].bbox.width() - 300.0).abs() < 1e-9);
    assert!((artwork.layers[0].bbox.height() - 199.5).abs() < 1e-9);

    // Re-emitting keeps the repeat instead of writing it out.
    let normalized = gerberx2::from_artwork::normalize_layer(&gerber, accuracy).unwrap();
    assert!(normalized.contains("%SRX300Y200I1J1*%"), "{normalized}");
    assert_eq!(normalized.matches("D03*").count(), 3);

    let error = GerberX2::parse("%FSLAX26Y26*%%MOMM*%%ABD20*%%SRX2Y2I1J1*%%SR*%%AB*%M02*")
        .unwrap_err()
        .to_string();
    assert!(error.contains("SR is not allowed inside an AB"), "{error}");
}

#[test]
fn rejects_counts_outside_the_format_limits() {
    for (body, message) in [
        ("%ADD10P,1X2000000000*%", "polygon vertices"),
        ("%ADD10P,1X2*%", "polygon vertices"),
        ("%ADD10P,1X4.5*%", "polygon vertices"),
        ("%AMM*5,1,13,0,0,1,0*%%ADD10M*%", "macro polygon vertices"),
        (
            "%AMM*4,1,100000000000000000000000000000,0,0,0*%%ADD10M*%",
            "macro outline vertices",
        ),
        ("%AMM*4,1,5001,0,0,0*%%ADD10M*%", "macro outline vertices"),
        (
            "%AMM*4,1,2,0,0,1,0,0,0,0*%%ADD10M*%",
            "macro outline vertices",
        ),
        ("%SRX100000Y100000I1J1*%", "SR repeats"),
        ("%SRX0Y1I1J1*%", "SR repeats"),
    ] {
        let error = GerberX2::parse(&format!("%FSLAX26Y26*%%MOMM*%{body}M02*"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{body}: {error}");
    }
    // The limits themselves are valid.
    GerberX2::parse("%FSLAX26Y26*%%MOMM*%%ADD10P,1X3*%%ADD11P,1X12*%M02*").unwrap();
}

#[test]
fn reads_deprecated_constructs_older_files_are_full_of() {
    // Words on their own lines inside one block, identity image commands, an
    // SR preamble that is never closed, G70/G90, G-codes fused with their
    // operation, D-codes without leading zeros, coordinates that repeat the
    // last operation, and a macro variable nobody set.
    let gerber = GerberX2::parse(
        "G04 RS-274X*\nG042 layers*\n%\nFSLAX24Y24*\nMOIN*\n%\n%IPPOS*%\n%LNTOP*%\n%INBoard, rev 2*%\n%ASAXBY*%\n%OFA0B0*%\n%SFA1.0B1.0*%\n%MIA0B0*%\n%IR0*%\n%SRX1Y1I0J0*%\n%AMDOT*\n1,1,$1,$2,0*\n%\n%ADD10C,0.0100*%\n%ADD11DOT,0.05*%\nG70*\nG90*\nG75*\nG54D10*\nG01X0Y0D02*\nX10000D01*\nY10000*\nG01X0Y10000D1*\nG54D11*\nG55X5000Y5000D3*\nX7000Y7000*\nM02*\n",
    )
    .unwrap();

    let objects = gerber.objects();
    assert_eq!(objects.len(), 5);
    assert!(matches!(
        objects[1].kind,
        ObjectKind::Draw { start, end, aperture: 10 }
            if close(start.x, 25.4) && close(start.y, 0.0) && close(end.x, 25.4) && close(end.y, 25.4)
    ));
    assert!(matches!(
        objects[2].kind,
        ObjectKind::Draw { end, .. } if close(end.x, 0.0) && close(end.y, 25.4)
    ));
    assert!(matches!(
        objects[3].kind,
        ObjectKind::Flash { at, aperture: 11 } if close(at.x, 12.7) && close(at.y, 12.7)
    ));
    assert!(matches!(
        objects[4].kind,
        ObjectKind::Flash { at, aperture: 11 } if close(at.x, 17.78) && close(at.y, 17.78)
    ));
    assert!(gerber.step_repeats().is_empty());
    // The unset `$2` centred the macro's circle on the origin.
    let artwork = extract(&gerber);
    assert!(close(artwork.objects[3].bbox.center().x, 12.7));

    // Altium ends every region with a bare move.
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%%MOMM*%G36*X0Y0D02*G01X1000000D01*Y1000000D01*X0Y0D01*D02*G37*M02*",
    )
    .unwrap();
    assert!(matches!(
        &gerber.objects()[0].kind,
        ObjectKind::Region { contours } if contours.len() == 1
    ));

    // An omitted arc offset is zero, and an arc with no offset at all is
    // the straight segment: Altium leaves arc mode on after a region.
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%%MOMM*%%ADD10C,0.1*%D10*G75*G03X0Y0D02*Y1000000J500000D01*X1000000D01*M02*",
    )
    .unwrap();
    assert!(matches!(
        gerber.objects()[0].kind,
        ObjectKind::Arc { center_offset, .. } if center_offset.x == 0.0 && center_offset.y == 0.5
    ));
    assert!(matches!(gerber.objects()[1].kind, ObjectKind::Draw { .. }));

    // G71 is `%MOMM`, and an unclosed step-repeat ends with the file.
    let gerber =
        GerberX2::parse("%FSLAX26Y26*%G71*%ADD10C,1*%D10*%SRX2Y1I5J0*%X1000000Y0D03*M02*").unwrap();
    assert!(matches!(
        gerber.objects()[0].kind,
        ObjectKind::Flash { at, .. } if close(at.x, 1.0)
    ));
    assert_eq!(gerber.step_repeats().len(), 1);
}

#[test]
fn rejects_deprecated_constructs_that_would_change_the_image() {
    for (body, message) in [
        ("G74*", "single-quadrant"),
        ("G91*", "incremental"),
        ("%IPNEG*%", "changes the image"),
        ("%ASAYBX*%", "changes the image"),
        ("%OFA1B0*%", "changes the image"),
        ("%SFA2B2*%", "changes the image"),
        ("%MIA1B0*%", "changes the image"),
        ("%IR90*%", "changes the image"),
        ("%ADD10C,1*%D10*X0Y0*", "require a previous operation"),
        ("%ADD10C,1*%D10*X0Y0D04*", "invalid D-code"),
        ("%ADD10C,1*%X0Y0D10*", "invalid D-code"),
    ] {
        let error = GerberX2::parse(&format!("%FSLAX26Y26*%%MOMM*%{body}M02*"))
            .unwrap_err()
            .to_string();
        assert!(error.contains(message), "{body}: {error}");
    }
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
fn extracts_render_artwork() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%TF.FileFunction,Copper,L1,Top*%\n%ADD10C,0.2*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nX1000000Y1000000D03*\nM02*\n",
    )
    .unwrap();

    let geometry = extract(&gerber);
    assert_eq!(geometry.layers[0].meta, vec!["Copper", "L1", "Top"]);
    assert_eq!(geometry.objects.len(), 2);
    assert!(geometry.arena.paths.iter().any(|path| path.is_stroked()));
    assert!(!geometry.layers[0].bbox.is_empty());
}

#[test]
fn composed_area_follows_polarity_and_paint_order() {
    for (body, expected, tolerance, why) in [
        (
            "%ADD10R,2.0X2.0*%%ADD11C,1.0*%D10*X0Y0D03*%LPC*%D11*X0Y0D03*",
            4.0 - std::f64::consts::PI * 0.25,
            0.02,
            "a clear flash cuts what is under it",
        ),
        (
            "%ADD10R,4.0X4.0*%%ADD11R,2.0X2.0*%D10*X0Y0D03*X0Y0D03*%LPC*%D11*X0Y0D03*",
            12.0,
            1e-9,
            "a clear flash cuts through overlapping dark runs",
        ),
        (
            "G36*G01*X0Y0D02*X4000000Y0D01*X4000000Y4000000D01*X0Y4000000D01*X0Y0D01*X1000000Y1000000D02*X1000000Y3000000D01*X3000000Y3000000D01*X3000000Y1000000D01*X1000000Y1000000D01*G37*",
            16.0,
            1e-9,
            "region contours fill independently, whatever their winding",
        ),
        (
            "%AMORDERED*21,1,4,4,0,0,0*21,0,2,2,0,0,0*21,1,1,1,0,0,0*%%ADD10ORDERED*%D10*X0Y0D03*",
            13.0,
            1e-9,
            "macro primitives paint in order",
        ),
    ] {
        let gerber = GerberX2::parse(&format!("%FSLAX26Y26*%%MOMM*%{body}M02*")).unwrap();
        let area =
            pcb_ir::dialects::artwork::compare::summarize(&extract(&gerber), Resolution::default())
                .unwrap()
                .area_mm2;
        assert!(
            (area - expected).abs() <= tolerance,
            "{why}: area was {area}"
        );
    }
}

#[test]
fn macro_flashes_share_one_composed_aperture() {
    let accuracy = GeometryAccuracy::default();
    // KiCad's rounded rectangle: an outline, four corner circles and four
    // edge lines, minus an exposure-off hole that only the macro may see.
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%AMRoundRect*\n4,1,4,0.4,-0.5,0.4,0.5,-0.4,0.5,-0.4,-0.5,0.4,-0.5,0*\n1,1,0.2,0.4,0.5*\n1,1,0.2,-0.4,0.5*\n1,1,0.2,-0.4,-0.5*\n1,1,0.2,0.4,-0.5*\n20,1,0.2,0.4,0.5,-0.4,0.5,0*\n20,1,0.2,-0.4,0.5,-0.4,-0.5,0*\n20,1,0.2,-0.4,-0.5,0.4,-0.5,0*\n20,1,0.2,0.4,-0.5,0.4,0.5,0*\n21,0,0.2,0.2,0,0,0*\n%\n%ADD10R,4X4*%\n%ADD11RoundRect*%\nD10*\nX1000000Y0D03*\nD11*\nX0Y0D03*\n%LR90*%\nX2000000Y0D03*\n%LPC*%\n%LR0*%\nX1000000Y1000000D03*\nM02*\n",
    )
    .unwrap();

    let geometry = extract(&gerber);
    assert_eq!(geometry.apertures.len(), 2);
    assert!(
        geometry
            .objects
            .iter()
            .all(|object| matches!(object.geometry, Geometry::Flash { .. }))
    );
    // The dark flashes lie inside the square; the clear one removes its own
    // image but not the square showing through its hole.
    let pad = 1.0 * 1.2 - (4.0 - std::f64::consts::PI) * 0.01 - 0.04;
    let area = pcb_ir::dialects::artwork::compare::summarize(&geometry, Resolution::default())
        .unwrap()
        .area_mm2;
    assert!((area - (16.0 - pad)).abs() < 0.001, "area was {area}");

    let normalized = gerberx2::from_artwork::normalize_layer(&gerber, accuracy).unwrap();
    assert_eq!(normalized.matches("D03*").count(), 4);
    assert!(!normalized.contains("G36*"));
}

#[test]
fn extraction_applies_scaling_to_circular_draw_width() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10C,0.2*%\n%LS2*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nM02*\n",
    )
    .unwrap();

    let geometry = extract(&gerber);
    let path = geometry
        .arena
        .paths
        .iter()
        .find(|path| path.is_stroked())
        .unwrap();
    assert!(close(path.stroke().unwrap().width, 0.4));
}

#[test]
fn extraction_flips_mirrored_aperture_arc_direction() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10O,2.0X1.0*%\n%LMX*%\nD10*\nX0Y0D03*\nM02*\n",
    )
    .unwrap();

    let geometry = extract(&gerber);
    let Geometry::Flash {
        aperture,
        transform,
    } = geometry.objects[0].geometry
    else {
        panic!("a flash stays a flash");
    };
    let placed = geometry.apertures[aperture as usize].contours()[0]
        .clone()
        .transformed(transform);
    let arc = placed
        .cmds
        .iter()
        .find(|cmd| cmd.op == pcb_ir::geom::PathOp::ArcTo)
        .unwrap();
    assert!(arc.clockwise);
}

#[test]
fn extracts_non_circular_aperture_sweeps_without_diagnostics() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%ADD10R,0.2X0.4*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nM02*\n",
    )
    .unwrap();

    let geometry = extract(&gerber);
    assert!(geometry.diagnostics.is_empty());
    assert_eq!(geometry.objects.len(), 1);
    assert!(geometry.arena.paths[0].is_filled());
}

#[test]
fn renders_profile_gerber_as_black_board_outline() {
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%\n%MOMM*%\n%TF.FileFunction,Profile,NP*%\n%ADD10C,0.1*%\nD10*\nG01*\nX0Y0D02*\nX1000000Y0D01*\nX1000000Y1000000D01*\nX0Y1000000D01*\nX0Y0D01*\nM02*\n",
    )
    .unwrap();

    let geometry = extract(&gerber);
    let mask =
        pcb_ir::dialects::artwork::compose_to_mask(&geometry, Resolution::default()).unwrap();
    let svg = pcb_ir::render::svg(&mask, &pcb_ir::render::RenderOptions::default());

    assert!(svg.contains("fill='none' stroke='#000000'"));
    assert!(svg.contains("data-board-outline='true'"));
    assert!(!svg.contains("fill='#606060'"));
}

#[test]
fn writes_polygon_hole_with_explicit_zero_rotation() {
    let layer = GerberLayer {
        apertures: vec![WriterAperture {
            code: 10,
            template: WriterApertureTemplate::Polygon {
                outer_diameter: 2.0,
                vertices: 6,
                rotation_degrees: None,
                hole_diameter: Some(0.5),
            },
            attributes: AttributeSets::EMPTY,
        }],
        objects: vec![flash(0.0, 0.0)],
        ..GerberLayer::default()
    };

    let output = written(&layer);
    assert!(output.contains("%ADD10P,2X6X0X0.5*%"));

    let parsed = GerberX2::parse(&output).unwrap();
    assert!(matches!(
        parsed.aperture_definitions()[0].template,
        ApertureTemplate::Polygon {
            rotation_degrees: Some(rotation),
            hole_diameter: Some(hole),
            ..
        } if close(rotation, 0.0) && close(hole, 0.5)
    ));
}

fn extract(gerber: &GerberX2) -> GerberArtworkDocument {
    gerberx2::geometry::extract_document(gerber, GeometryAccuracy::default()).unwrap()
}

/// The layer's objects with every step-repeat and block aperture imaged out.
fn imaged(gerber: &GerberX2) -> Vec<Object<gerberx2::geometry::GerberObjectMeta>> {
    pcb_ir::dialects::artwork::expand_instances(&extract(gerber)).objects
}

/// A layer whose one aperture, D10, is a circle.
fn circle_layer(diameter: f64, objects: Vec<WriterObject>) -> GerberLayer {
    GerberLayer {
        apertures: vec![WriterAperture {
            code: 10,
            template: WriterApertureTemplate::Circle {
                diameter,
                hole_diameter: None,
            },
            attributes: AttributeSets::EMPTY,
        }],
        objects,
        ..GerberLayer::default()
    }
}

fn flash(x: f64, y: f64) -> WriterObject {
    WriterObject::dark(ObjectKind::Flash {
        at: Point { x, y },
        aperture: 10,
    })
}

/// Write `layer`; the MakerPnP `gerber_parser` crate, an independent syntax
/// oracle, must accept everything our writer emits.
fn written(layer: &GerberLayer) -> String {
    let content = gerberx2::write_layer(layer).unwrap();
    let reader = std::io::BufReader::new(content.as_bytes());
    if let Err((_, error)) = gerber_parser::parse(reader) {
        panic!("external gerber_parser rejected our output: {error:?}\n---\n{content}");
    }
    content
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9
}

#[test]
fn shaped_draws_sweep_continuously_with_recorded_accuracy() {
    use pcb_ir::geom::{ContourSet, FillRule, Point};
    for (draw, witness) in [
        ("G01*X0Y0D02*X100000Y0D01*", Point::new(0.0125, 0.0)),
        (
            "G75*X100000Y0D02*G03*X0Y100000I-100000J0D01*",
            Point::new(0.1 / 2.0_f64.sqrt(), 0.1 / 2.0_f64.sqrt()),
        ),
    ] {
        let gerber = GerberX2::parse(&format!(
            "%FSLAX26Y26*%%MOMM*%%AMthin*21,1,0.002,0.002,0,0,0*%%ADD10thin*%D10*{draw}M02*"
        ))
        .unwrap();
        for budget in [0.001, 0.0001] {
            let accuracy = GeometryAccuracy::new(budget).unwrap();
            let doc = gerberx2::geometry::extract_document(&gerber, accuracy).unwrap();
            let path = &doc.arena.paths[0];
            let region = ContourSet::from_contours(
                &doc.arena.path_contours(path),
                FillRule::NonZero,
                Resolution::new(0.0, accuracy),
            )
            .unwrap();
            assert_eq!(region.connected_components().len(), 1);
            assert!(region.contains_point(witness));
            assert!(region.uncertainty_mm > 0.0 && region.uncertainty_mm <= budget);
        }
    }
    let gerber = GerberX2::parse(
        "%FSLAX26Y26*%%MOMM*%%AMhole*21,1,0.1,0.1,0,0,0*21,0,0.05,0.05,0,0,0*%%ADD10hole*%D10*G01*X0Y0D02*X10000Y0D01*M02*"
    ).unwrap();
    let accuracy = GeometryAccuracy::default();
    let doc = extract(&gerber);
    let region = ContourSet::from_contours(
        &doc.arena.path_contours(&doc.arena.paths[0]),
        FillRule::NonZero,
        Resolution::new(0.0, accuracy),
    )
    .unwrap();
    assert!(!region.contains_point(Point::new(0.005, 0.0)));
    assert!(region.contains_point(Point::new(0.005, 0.04)));
}

#[test]
fn an_aperture_hole_wider_than_its_shape_removes_material_and_adds_none() {
    use pcb_ir::geom::Point;
    let image = |aperture: &str| {
        let gerber = GerberX2::parse(&format!(
            "%FSLAX26Y26*%%MOMM*%%ADD10{aperture}*%D10*X0Y0D03*M02*"
        ))
        .unwrap();
        let doc = extract(&gerber);
        let resolution = Resolution::default();
        let (mut layers, _) =
            pcb_ir::dialects::artwork::compose_owner_regions(&doc, |_| Some(()), resolution)
                .unwrap();
        layers.pop().unwrap().pop().map(|(_, region)| region)
    };

    // The disc covers the 1.0 x 0.4 rectangle's middle and overhangs its
    // long edges: only the rectangle's two ends are copper.
    let ends = image("R,1.0X0.4X0.5").unwrap();
    assert_eq!(ends.connected_components().len(), 2);
    assert!(ends.contains_point(Point::new(0.4, 0.0)));
    assert!(!ends.contains_point(Point::new(0.0, 0.0)));
    assert!(!ends.contains_point(Point::new(0.0, 0.23)));
    // A hole swallowing its circle leaves nothing.
    assert!(image("C,1.0X1.5").is_none());
    // A hole that fits stays a standard aperture.
    let ring = image("C,1.0X0.5").unwrap();
    assert!(ring.contains_point(Point::new(0.4, 0.0)) && !ring.contains_point(Point::ZERO));
}
