use pcb_kicad_sch::{
    Graphic, GraphicKind, Point, SchDocument, SchItem, SchPage, connectivity::ConnectivityGraph,
    parse_kicad_sch_page, patch_page_source,
};

const SOURCE: &str = include_str!("../test-data/kicad-10/sheet-graphics.kicad_sch");

fn graphics(page: &SchPage) -> impl Iterator<Item = &Graphic> {
    page.items.iter().filter_map(|item| match item {
        SchItem::Graphic(graphic) => Some(graphic),
        _ => None,
    })
}

#[test]
fn graphics_are_typed_identified_and_round_trip_with_text_styles() {
    let document = SchDocument::from_kicad_sch(SOURCE).unwrap();
    let page = &document.pages[0];
    assert_eq!(graphics(page).count(), 9);
    for graphic in graphics(page) {
        assert_eq!(
            SchItem::Graphic(graphic.clone()).id(),
            Some(graphic.id.as_str())
        );
    }
    let title = graphics(page)
        .find(|graphic| graphic.id == "title")
        .unwrap();
    let GraphicKind::Text(text) = &title.kind else {
        panic!("expected text")
    };
    assert!(text.effects.bold);
    assert_eq!(text.effects.thickness, Some(0.3));
    assert_eq!(text.at, Point::new(33.0, 34.0));
    let hidden = graphics(page)
        .find(|graphic| graphic.id == "hidden")
        .unwrap();
    assert!(matches!(&hidden.kind, GraphicKind::Text(text) if text.hidden));
    let multiline = graphics(page)
        .find(|graphic| graphic.id == "multiline")
        .unwrap();
    assert!(matches!(&multiline.kind, GraphicKind::Text(text)
        if text.angle == 22.5 && text.effects.italic && text.text.contains('\n')));
    let serialized = document.to_kicad_sch().unwrap();
    assert_eq!(SchDocument::from_kicad_sch(&serialized).unwrap(), document);
    assert_eq!(patch_page_source(SOURCE, page).unwrap(), None);
    let json = serde_json::to_string(&document).unwrap();
    assert_eq!(
        serde_json::from_str::<SchDocument>(&json).unwrap(),
        document
    );
}

#[test]
fn translations_move_every_point_without_changing_dimensions_or_styling() {
    let mut document = SchDocument::from_kicad_sch(SOURCE).unwrap();
    let before = document.clone();
    for item in &mut document.pages[0].items {
        if let SchItem::Graphic(graphic) = item {
            let at = graphic.anchor().unwrap();
            graphic.translate(Point::new(10.0, -20.0));
            assert_eq!(graphic.anchor(), Some(Point::new(at.x + 10.0, at.y - 20.0)));
        }
    }
    for (after, before) in graphics(&document.pages[0]).zip(graphics(&before.pages[0])) {
        assert_eq!(after.id, before.id);
        assert_eq!(after.unsupported, before.unsupported);
        match (&after.kind, &before.kind) {
            (GraphicKind::Rectangle { start, end }, _) => {
                assert_eq!(
                    (*start, *end),
                    (Point::new(40.0, 10.0), Point::new(130.0, 50.0))
                );
            }
            (GraphicKind::Polyline { points }, _) => assert_eq!(
                points,
                &vec![
                    Point::new(44.0, 41.0),
                    Point::new(58.0, 41.0),
                    Point::new(58.0, 35.0),
                    Point::new(70.0, 35.0)
                ]
            ),
            (GraphicKind::Circle { center, radius }, _) => {
                assert_eq!((*center, *radius), (Point::new(86.0, 38.0), 6.0));
            }
            (GraphicKind::Arc { start, mid, end }, _) => assert_eq!(
                (*start, *mid, *end),
                (
                    Point::new(102.0, 41.0),
                    Point::new(109.0, 34.0),
                    Point::new(116.0, 41.0)
                )
            ),
            (GraphicKind::Text(after), GraphicKind::Text(before))
            | (
                GraphicKind::TextBox { text: after, .. },
                GraphicKind::TextBox { text: before, .. },
            ) => {
                let mut normalized = after.clone();
                normalized.at = before.at;
                assert_eq!(normalized, *before);
            }
            _ => panic!("translation changed graphic kind"),
        }
        if let (GraphicKind::TextBox { size: a, .. }, GraphicKind::TextBox { size: b, .. }) =
            (&after.kind, &before.kind)
        {
            assert_eq!(a, b);
        }
    }
    // Non-electrical geometry never joins or splits nets, even over electrical items.
    assert_eq!(
        ConnectivityGraph::from_kicad(&before).unwrap(),
        ConnectivityGraph::from_kicad(&document).unwrap()
    );
}

#[test]
fn actual_source_patches_persist_graphics_only_and_mixed_moves() {
    for mixed in [false, true] {
        let mut page = parse_kicad_sch_page(None, SOURCE).unwrap();
        for item in &mut page.items {
            match item {
                SchItem::Graphic(graphic) if graphic.id != "hidden" => {
                    graphic.translate(Point::new(5.08, -2.54))
                }
                SchItem::Wire(wire) if mixed => {
                    wire.a.x += 5.08;
                    wire.b.x += 5.08;
                    wire.a.y -= 2.54;
                    wire.b.y -= 2.54;
                }
                SchItem::Label(label) if mixed => {
                    label.at.x += 5.08;
                    label.at.y -= 2.54;
                }
                _ => {}
            }
        }
        let patched = patch_page_source(SOURCE, &page)
            .unwrap()
            .expect("movement must produce a write");
        let reloaded = parse_kicad_sch_page(None, &patched).unwrap();
        assert_eq!(reloaded, page);
        for graphic in graphics(&page) {
            assert_eq!(
                reloaded
                    .items
                    .iter()
                    .filter(|item| item.id() == Some(&graphic.id))
                    .count(),
                1
            );
        }
        assert_eq!(patch_page_source(&patched, &page).unwrap(), None);
        // Unchanged typed graphics, unknown nodes, and unrelated electrical items
        // retain their exact original source, not just equivalent values.
        let root = pcb_sexpr::parse(SOURCE).unwrap();
        for node in root.as_list().unwrap().iter().skip(1) {
            let Some(items) = node.as_list() else {
                continue;
            };
            let tag = items.first().and_then(pcb_sexpr::Sexpr::as_sym);
            let hidden = pcb_sexpr::find_child_list(items, "uuid")
                .and_then(|uuid| uuid.get(1))
                .and_then(pcb_sexpr::Sexpr::as_atom)
                == Some("hidden");
            if hidden
                || matches!(tag, Some("image" | "sheet_instances"))
                || (!mixed && matches!(tag, Some("wire" | "label")))
            {
                assert!(patched.contains(&SOURCE[node.span.start..node.span.end]));
            }
        }
    }
}

#[test]
fn source_patching_supports_graphic_addition_removal_and_duplicate_detection() {
    let mut page = parse_kicad_sch_page(None, SOURCE).unwrap();
    let mut copy = graphics(&page)
        .find(|graphic| graphic.id == "block")
        .unwrap()
        .clone();
    copy.id = "new-block".into();
    copy.translate(Point::new(10.0, 0.0));
    page.items.retain(|item| item.id() != Some("block"));
    page.items.push(SchItem::Graphic(copy.clone()));
    let patched = patch_page_source(SOURCE, &page).unwrap().unwrap();
    let reloaded = parse_kicad_sch_page(None, &patched).unwrap();
    assert!(!reloaded.items.iter().any(|item| item.id() == Some("block")));
    assert!(graphics(&reloaded).any(|graphic| graphic == &copy));
    assert_eq!(patch_page_source(&patched, &page).unwrap(), None);
    page.items.push(SchItem::Graphic(copy));
    assert!(
        patch_page_source(&patched, &page)
            .unwrap_err()
            .to_string()
            .contains("duplicate")
    );
}

#[test]
fn malformed_graphic_geometry_is_rejected_instead_of_losing_data() {
    for graphic in [
        "(rectangle (start 1 2) (uuid r))",
        "(polyline (pts (xy 1 2)) (uuid p))",
        "(circle (center 1 2) (radius nope) (uuid c))",
        "(arc (start 1 2) (end 3 4) (uuid a))",
        "(text \"note\" (at 1 2 nope) (uuid t))",
        "(text_box \"note\" (at 1 2 90) (uuid b))",
    ] {
        let source = format!("(kicad_sch (version 20260306) (uuid root) {graphic})");
        assert!(parse_kicad_sch_page(None, &source).is_err(), "{graphic}");
    }
}
