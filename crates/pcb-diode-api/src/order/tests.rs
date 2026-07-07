use super::*;

// ---------------------------------------------------------------------------
// Board-identity resolution
// ---------------------------------------------------------------------------

#[test]
fn parses_standard_board_repository() {
    assert_eq!(
        parse_board_repository("code.diode.computer/demo/b/DM0002"),
        Some(("demo".to_string(), "DM0002".to_string()))
    );
}

#[test]
fn parses_repository_with_extra_host_segments() {
    // Anchoring on the `b` marker keeps resolution correct with nested paths.
    assert_eq!(
        parse_board_repository("code.diode.computer/org/team/b/DM9999"),
        Some(("team".to_string(), "DM9999".to_string()))
    );
}

#[test]
fn rejects_repository_without_board_marker() {
    assert_eq!(parse_board_repository("github.com/diodeinc/registry"), None);
    assert_eq!(parse_board_repository("b/DM0002"), None); // no workspace before `b`
    assert_eq!(parse_board_repository("demo/b"), None); // no board after `b`
}

#[test]
fn resolves_identity_from_repository() {
    let id = resolve_board_identity(Some("code.diode.computer/demo/b/DM0002"), None, None).unwrap();
    assert_eq!(id.workspace, "demo");
    assert_eq!(id.board, "DM0002");
}

#[test]
fn flags_override_repository() {
    let id = resolve_board_identity(
        Some("code.diode.computer/demo/b/DM0002"),
        Some("other-ws"),
        Some("DM1234"),
    )
    .unwrap();
    assert_eq!(id.workspace, "other-ws");
    assert_eq!(id.board, "DM1234");
}

#[test]
fn single_flag_overrides_only_that_field() {
    let id = resolve_board_identity(
        Some("code.diode.computer/demo/b/DM0002"),
        None,
        Some("DM1234"),
    )
    .unwrap();
    assert_eq!(id.workspace, "demo");
    assert_eq!(id.board, "DM1234");
}

#[test]
fn both_flags_work_without_a_repository() {
    let id = resolve_board_identity(None, Some("demo"), Some("DM0002")).unwrap();
    assert_eq!(id.workspace, "demo");
    assert_eq!(id.board, "DM0002");
}

#[test]
fn errors_outside_workspace_with_no_flags() {
    let err = resolve_board_identity(None, None, None).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("--workspace"), "message was: {msg}");
    assert!(msg.contains("--board"), "message was: {msg}");
}

#[test]
fn errors_when_partial_flag_cannot_be_completed() {
    // Only workspace flag, no repository to supply the board.
    assert!(resolve_board_identity(None, Some("demo"), None).is_err());
    // Only board flag, no repository to supply the workspace.
    assert!(resolve_board_identity(None, None, Some("DM0002")).is_err());
}

// ---------------------------------------------------------------------------
// MPN normalization
// ---------------------------------------------------------------------------

#[test]
fn normalization_matches_backend_rules() {
    assert_eq!(normalize_bom_lookup_mpn("  stm32f407 "), "STM32F407");
    assert_eq!(normalize_bom_lookup_mpn("GRM188R71H"), "GRM188R71H");
    // Only A-Z, 0-9 and `.` survive.
    assert_eq!(normalize_bom_lookup_mpn("PESD2CAN,215"), "PESD2CAN215");
    assert_eq!(normalize_bom_lookup_mpn("AT86RF212B.ZU"), "AT86RF212B.ZU");
    assert_eq!(normalize_bom_lookup_mpn("mc34063a/d"), "MC34063AD");
    assert_eq!(normalize_bom_lookup_mpn("part number 42"), "PARTNUMBER42");
}

// ---------------------------------------------------------------------------
// Selection-join precedence
// ---------------------------------------------------------------------------

fn sample_bom() -> Bom {
    Bom {
        id: Some("bom_1".into()),
        lines: vec![
            // Line with a default selection AND an override -> override wins.
            BomLine {
                id: "line_override".into(),
                mpn: Some("STM32F407VGT6".into()),
                manufacturer: Some("STMicroelectronics".into()),
                package: Some("LQFP-100".into()),
                value: None,
                designator: Some("U1".into()),
                path: Some("root.U1".into()),
                alternatives: vec![Alternative {
                    mpn: Some("STM32F407VGT7".into()),
                    manufacturer: Some("ST".into()),
                }],
                match_status: Some("matched".into()),
                offers: vec![
                    Offer {
                        id: "offer_default".into(),
                        mpn: Some("STM32F407VGT6".into()),
                        manufacturer: Some("STMicroelectronics".into()),
                        distributor: Some("lcsc".into()),
                        stock: Some(1200),
                        price: Some(8.50),
                    },
                    Offer {
                        id: "offer_override".into(),
                        mpn: Some("STM32F407VET6".into()),
                        manufacturer: Some("STMicroelectronics".into()),
                        distributor: Some("digikey".into()),
                        stock: Some(50),
                        price: Some(9.10),
                    },
                ],
                selected_offer_id: Some("offer_default".into()),
            },
            // Line with only a default selection.
            BomLine {
                id: "line_default".into(),
                mpn: Some("GRM188R71H104KA93D".into()),
                manufacturer: Some("Murata".into()),
                package: Some("0603".into()),
                value: Some("100nF".into()),
                designator: Some("C1".into()),
                path: Some("root.C1".into()),
                alternatives: vec![],
                match_status: Some("matched".into()),
                offers: vec![Offer {
                    id: "offer_c1".into(),
                    mpn: Some("GRM188R71H104KA93D".into()),
                    manufacturer: Some("Murata".into()),
                    distributor: Some("lcsc".into()),
                    stock: Some(500000),
                    price: Some(0.01),
                }],
                selected_offer_id: Some("offer_c1".into()),
            },
            // Line with no selection at all.
            BomLine {
                id: "line_none".into(),
                mpn: Some("XYZ-UNMATCHED".into()),
                manufacturer: None,
                package: None,
                value: None,
                designator: Some("J1".into()),
                path: Some("root.J1".into()),
                alternatives: vec![],
                match_status: Some("unmatched".into()),
                offers: vec![],
                selected_offer_id: None,
            },
        ],
    }
}

fn sample_selections() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("line_override".to_string(), "offer_override".to_string());
    map
}

#[test]
fn override_beats_default() {
    let rows = build_order_bom_rows(&sample_bom(), &sample_selections());
    let row = &rows[0];
    assert_eq!(row.selection_source, SelectionSource::OrderOverride);
    assert_eq!(row.selected_offer_id.as_deref(), Some("offer_override"));
    assert_eq!(row.selected_mpn.as_deref(), Some("STM32F407VET6"));
}

#[test]
fn default_used_when_no_override() {
    let rows = build_order_bom_rows(&sample_bom(), &sample_selections());
    let row = &rows[1];
    assert_eq!(row.selection_source, SelectionSource::Default);
    assert_eq!(row.selected_offer_id.as_deref(), Some("offer_c1"));
    assert_eq!(row.selected_mpn.as_deref(), Some("GRM188R71H104KA93D"));
}

#[test]
fn none_when_neither_override_nor_default() {
    let rows = build_order_bom_rows(&sample_bom(), &sample_selections());
    let row = &rows[2];
    assert_eq!(row.selection_source, SelectionSource::None);
    assert!(row.selected_offer_id.is_none());
    assert!(row.selected_mpn.is_none());
    assert!(row.selected_manufacturer.is_none());
}

// ---------------------------------------------------------------------------
// BOM id resolution ("order has no BOM")
// ---------------------------------------------------------------------------

fn order_with_bom(bom_id: Option<&str>) -> OrderDetail {
    OrderDetail {
        id: "ord_1".into(),
        name: None,
        status: None,
        quantity: None,
        provider: None,
        created_at: None,
        release_id: None,
        release_version: None,
        bom_id: bom_id.map(str::to_string),
        quote: None,
        timeline: vec![],
        shipping_location_id: None,
    }
}

#[test]
fn resolves_present_bom_id() {
    let order = order_with_bom(Some("bom_555"));
    assert_eq!(resolve_order_bom_id(&order).unwrap(), "bom_555");
}

#[test]
fn errors_when_order_has_no_bom() {
    for missing in [order_with_bom(None), order_with_bom(Some(""))] {
        let err = resolve_order_bom_id(&missing).unwrap_err();
        assert!(err.to_string().contains("has no BOM"), "was: {err}");
    }
}

// ---------------------------------------------------------------------------
// --mismatches-only normalization edge cases
// ---------------------------------------------------------------------------

fn row_with(design_mpn: Option<&str>, selected_mpn: Option<&str>) -> OrderBomRow {
    OrderBomRow {
        bom_line_id: "line".into(),
        design: DesignEntry {
            mpn: design_mpn.map(str::to_string),
            manufacturer: None,
            package: None,
            value: None,
            designator: Some("U1".into()),
            path: None,
            alternatives: vec![],
        },
        match_status: None,
        offers: vec![],
        selected_offer_id: selected_mpn.map(|_| "offer".to_string()),
        selection_source: if selected_mpn.is_some() {
            SelectionSource::Default
        } else {
            SelectionSource::None
        },
        selected_mpn: selected_mpn.map(str::to_string),
        selected_manufacturer: None,
    }
}

#[test]
fn case_only_difference_is_not_a_mismatch() {
    assert!(!row_with(Some("stm32f407"), Some("STM32F407")).is_mpn_mismatch());
}

#[test]
fn punctuation_only_difference_is_not_a_mismatch() {
    // Comma / slash / dash are stripped by normalization.
    assert!(!row_with(Some("PESD2CAN,215"), Some("PESD2CAN215")).is_mpn_mismatch());
    assert!(!row_with(Some("MC34063A/D"), Some("MC34063AD")).is_mpn_mismatch());
}

#[test]
fn genuinely_different_mpn_is_a_mismatch() {
    assert!(row_with(Some("STM32F407VGT6"), Some("STM32F407VET6")).is_mpn_mismatch());
}

#[test]
fn missing_selection_is_excluded_from_mismatches() {
    assert!(!row_with(Some("STM32F407VGT6"), None).is_mpn_mismatch());
}

#[test]
fn missing_design_mpn_is_excluded_from_mismatches() {
    assert!(!row_with(None, Some("STM32F407VGT6")).is_mpn_mismatch());
}

#[test]
fn mismatches_only_filter_keeps_only_true_mismatches() {
    let bom = Bom {
        id: Some("bom".into()),
        lines: vec![
            // Genuine mismatch (override to a different part).
            BomLine {
                id: "l1".into(),
                mpn: Some("STM32F407VGT6".into()),
                manufacturer: None,
                package: None,
                value: None,
                designator: Some("U1".into()),
                path: None,
                alternatives: vec![],
                match_status: None,
                offers: vec![Offer {
                    id: "o1".into(),
                    mpn: Some("STM32F407VET6".into()),
                    manufacturer: None,
                    distributor: None,
                    stock: None,
                    price: None,
                }],
                selected_offer_id: None,
            },
            // Punctuation-only difference -> not a mismatch.
            BomLine {
                id: "l2".into(),
                mpn: Some("MC34063A/D".into()),
                manufacturer: None,
                package: None,
                value: None,
                designator: Some("U2".into()),
                path: None,
                alternatives: vec![],
                match_status: None,
                offers: vec![Offer {
                    id: "o2".into(),
                    mpn: Some("MC34063AD".into()),
                    manufacturer: None,
                    distributor: None,
                    stock: None,
                    price: None,
                }],
                selected_offer_id: Some("o2".into()),
            },
        ],
    };
    let mut selections = BTreeMap::new();
    selections.insert("l1".to_string(), "o1".to_string());

    let mut rows = build_order_bom_rows(&bom, &selections);
    rows.retain(OrderBomRow::is_mpn_mismatch);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bom_line_id, "l1");
}

// ---------------------------------------------------------------------------
// JSON snapshot output against recorded / mocked API responses
// ---------------------------------------------------------------------------

const ORDERS_LIST_FIXTURE: &str = r#"{
  "orders": [
    {
      "id": "ord_abc123",
      "name": "Prototype run",
      "status": "in_production",
      "quantity": 10,
      "releaseVersion": "v1.2.3",
      "provider": "jlcpcb",
      "createdAt": "2024-05-01T12:30:00Z"
    },
    {
      "id": "ord_def456",
      "status": "quoted",
      "quantity": 100,
      "provider": "pcbway",
      "createdAt": "2024-06-15T09:00:00Z"
    }
  ]
}"#;

const ORDER_DETAIL_FIXTURE: &str = r#"{
  "id": "ord_abc123",
  "name": "Prototype run",
  "status": "in_production",
  "quantity": 10,
  "provider": "jlcpcb",
  "createdAt": "2024-05-01T12:30:00Z",
  "releaseId": "rel_789",
  "releaseVersion": "v1.2.3",
  "bomId": "bom_555",
  "quote": {
    "id": "quote_1",
    "status": "accepted",
    "currency": "USD",
    "unitPrice": 12.34,
    "total": 123.40,
    "leadTimeDays": 14
  },
  "timeline": [
    { "status": "created", "timestamp": "2024-05-01T12:30:00Z" },
    { "status": "quoted", "timestamp": "2024-05-02T08:00:00Z", "note": "auto-quoted" },
    { "status": "in_production", "timestamp": "2024-05-03T10:15:00Z" }
  ],
  "shippingLocationId": "loc_42"
}"#;

const BOM_FIXTURE: &str = r#"{
  "id": "bom_555",
  "lines": [
    {
      "id": "line_override",
      "mpn": "STM32F407VGT6",
      "manufacturer": "STMicroelectronics",
      "package": "LQFP-100",
      "designator": "U1",
      "path": "root.U1",
      "alternatives": [{ "mpn": "STM32F407VGT7", "manufacturer": "ST" }],
      "matchStatus": "matched",
      "selectedOfferId": "offer_default",
      "offers": [
        {
          "id": "offer_default",
          "mpn": "STM32F407VGT6",
          "manufacturer": "STMicroelectronics",
          "distributor": "lcsc",
          "stock": 1200,
          "price": 8.5
        },
        {
          "id": "offer_override",
          "mpn": "STM32F407VET6",
          "manufacturer": "STMicroelectronics",
          "distributor": "digikey",
          "stock": 50,
          "price": 9.1
        }
      ]
    },
    {
      "id": "line_default",
      "mpn": "GRM188R71H104KA93D",
      "manufacturer": "Murata",
      "package": "0603",
      "value": "100nF",
      "designator": "C1",
      "path": "root.C1",
      "matchStatus": "matched",
      "selectedOfferId": "offer_c1",
      "offers": [
        {
          "id": "offer_c1",
          "mpn": "GRM188R71H104KA93D",
          "manufacturer": "Murata",
          "distributor": "lcsc",
          "stock": 500000,
          "price": 0.01
        }
      ]
    },
    {
      "id": "line_none",
      "mpn": "XYZ-UNMATCHED",
      "designator": "J1",
      "path": "root.J1",
      "matchStatus": "unmatched",
      "offers": []
    }
  ]
}"#;

const SELECTIONS_FIXTURE: &str = r#"{ "selections": { "line_override": "offer_override" } }"#;

#[test]
fn parses_orders_list_object_wrapper() {
    let orders = parse_orders_list(ORDERS_LIST_FIXTURE).unwrap();
    assert_eq!(orders.len(), 2);
    assert_eq!(orders[0].id, "ord_abc123");
    assert_eq!(orders[1].release_version, None);
}

#[test]
fn parses_orders_list_bare_array() {
    let orders = parse_orders_list(r#"[{"id":"ord_1"}]"#).unwrap();
    assert_eq!(orders.len(), 1);
    assert_eq!(orders[0].id, "ord_1");
}

#[test]
fn parses_selections_object_and_bare_map() {
    let wrapped = parse_selections(SELECTIONS_FIXTURE).unwrap();
    assert_eq!(
        wrapped.get("line_override").map(String::as_str),
        Some("offer_override")
    );

    let bare = parse_selections(r#"{ "line_a": "offer_a" }"#).unwrap();
    assert_eq!(bare.get("line_a").map(String::as_str), Some("offer_a"));

    let empty = parse_selections("null").unwrap();
    assert!(empty.is_empty());
}

#[test]
fn snapshot_orders_list_json() {
    let orders = parse_orders_list(ORDERS_LIST_FIXTURE).unwrap();
    insta::assert_json_snapshot!(orders);
}

#[test]
fn snapshot_order_detail_json() {
    let order: OrderDetail = serde_json::from_str(ORDER_DETAIL_FIXTURE).unwrap();
    insta::assert_json_snapshot!(order);
}

#[test]
fn snapshot_order_bom_report_json() {
    let bom: Bom = serde_json::from_str(BOM_FIXTURE).unwrap();
    let selections = parse_selections(SELECTIONS_FIXTURE).unwrap();
    let rows = build_order_bom_rows(&bom, &selections);
    let report = OrderBomReport {
        order_id: "ord_abc123".into(),
        bom_id: bom.id.clone().unwrap(),
        rows,
    };
    insta::assert_json_snapshot!(report);
}

#[test]
fn snapshot_order_bom_report_mismatches_only_json() {
    let bom: Bom = serde_json::from_str(BOM_FIXTURE).unwrap();
    let selections = parse_selections(SELECTIONS_FIXTURE).unwrap();
    let mut rows = build_order_bom_rows(&bom, &selections);
    rows.retain(OrderBomRow::is_mpn_mismatch);
    let report = OrderBomReport {
        order_id: "ord_abc123".into(),
        bom_id: bom.id.clone().unwrap(),
        rows,
    };
    // Only line_override is a real mismatch (VGT6 -> VET6).
    assert_eq!(report.rows.len(), 1);
    insta::assert_json_snapshot!(report);
}
