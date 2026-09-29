//! Explicit, source-preserving removal of provably isolated interior via lands.

use anyhow::{Context, Result, bail, ensure};
use ipc2581::edit::{Doc, Node};
use ipc2581::types::SetFeature;
use pcb_ir::geom::Resolution;
use pcb_ir::import::ipc2581::{import_design, is_copper};
use pcb_ir::import::unused_via_lands::isolated_via_lands;

/// Remove only explicit Pad elements whose every placed occurrence is isolated.
/// Padstack definitions, drills, endpoints, and all unrelated XML stay intact.
/// Returns the edited XML and number of source Pad elements removed (not copies).
pub fn remove_unused_via_lands(xml: &str, resolution: Resolution) -> Result<(String, usize)> {
    let doc = Doc::parse(xml)?;
    let ipc = ipc2581::Ipc2581::parse(xml)?;
    let design = import_design(&ipc, resolution)?;

    // The permissive general importer is not a completeness certificate. Check
    // a deliberately limited grammar before interpreting absent contacts.
    for name in [
        "DictionaryStandard",
        "DictionaryUser",
        "DictionaryLineDesc",
        "DictionaryFillDesc",
    ] {
        for node in doc.find_all(name) {
            check_geometry(&doc, node)?;
        }
    }
    let steps = doc.find_all("Step");
    let mut names = std::collections::HashSet::new();
    for &step in &steps {
        ensure!(
            names.insert(doc.attr(step, "name")),
            "Duplicate Step names prevent safe via cleanup"
        );
        for child in doc.children(step) {
            match doc.name(child) {
                "LayerFeature" => {
                    let name = doc
                        .attr(child, "layerRef")
                        .context("LayerFeature without layerRef")?;
                    let layer = design.layer_id(name).context("Undeclared feature layer")?;
                    let layer = &design.layer_definitions[layer.0 as usize];
                    if is_copper(layer.layer_function) || layer.layer_function.is_fabrication() {
                        check_geometry(&doc, child)?;
                    }
                }
                "PadStackDef" => check_geometry(&doc, child)?,
                "StepRepeat" => {
                    ensure!(
                        doc.children(child).is_empty(),
                        "Unsupported StepRepeat children"
                    );
                }
                "Datum"
                | "Profile"
                | "Package"
                | "Component"
                | "LogicalNet"
                | "PhyNetGroup"
                | "NonstandardAttribute" => {}
                name => bail!("Cannot prove via isolation: unsupported Step child {name}"),
            }
        }
    }
    let candidates = isolated_via_lands(&design, resolution)?;
    let mut edits = Vec::new();
    for id in candidates {
        let feature = design
            .feature_definition(id)
            .context("Missing candidate definition")?;
        let step_name = design.resolve(
            feature
                .source_step_ref
                .context("Candidate without source step")?,
        );
        let layer_name = design.resolve(
            feature
                .source_layer_ref
                .context("Candidate without source layer")?,
        );
        let step = steps
            .iter()
            .copied()
            .find(|&step| doc.attr(step, "name") == Some(step_name))
            .context("Missing source Step")?;
        let layers = doc
            .children(step)
            .into_iter()
            .filter(|&node| {
                doc.name(node) == "LayerFeature" && doc.attr(node, "layerRef") == Some(layer_name)
            })
            .collect::<Vec<_>>();
        // SourceRef numbers sets within a LayerFeature; repeated declarations
        // cannot be mapped unambiguously. Leave them alone.
        let [layer] = layers.as_slice() else { continue };
        let set = doc
            .children(*layer)
            .into_iter()
            .filter(|&node| doc.name(node) == "Set")
            .nth(feature.source.set_index as usize)
            .context("Missing source Set")?;
        // Features containers can expand into several parsed entries. Count
        // only standalone Pads in the parsed Set to recover the XML ordinal.
        let parsed_layer = ipc
            .ecad()
            .context("Missing Ecad")?
            .cad_data
            .steps
            .iter()
            .find(|s| ipc.resolve(s.name) == step_name)
            .and_then(|s| {
                s.layer_features
                    .iter()
                    .find(|l| ipc.resolve(l.layer_ref) == layer_name)
            })
            .context("Missing parsed source layer")?;
        let parsed_set = &parsed_layer.sets[feature.source.set_index as usize];
        let entries = parsed_set.features.slice(&parsed_layer.features);
        let index = feature.source.feature_index as usize;
        if !matches!(entries.get(index), Some(SetFeature::Pad(_)))
            || feature.placement_group.is_some()
        {
            continue;
        }
        let ordinal = entries[..index]
            .iter()
            .filter(|entry| matches!(entry, SetFeature::Pad(_)))
            .count();
        let pad = doc
            .children(set)
            .into_iter()
            .filter(|&node| doc.name(node) == "Pad")
            .nth(ordinal)
            .context("Missing source Pad")?;
        edits.push(doc.delete(pad));
    }
    let count = edits.len();
    let updated = if count == 0 {
        xml.to_owned()
    } else {
        doc.apply(edits)?
    };
    Ok((updated, count))
}

/// Closed grammar for the geometry paths we use. Unknown children must not be
/// silently ignored. In particular Xform on a Polygon is not supported by the
/// polygon importer, even though transforms elsewhere are supported.
fn check_geometry(doc: &Doc<'_>, node: Node) -> Result<()> {
    let children = doc.children(node);
    let allowed: &[&str] = match doc.name(node) {
        "DictionaryStandard" => &["EntryStandard"],
        "DictionaryUser" => &["EntryUser"],
        "DictionaryLineDesc" => &["EntryLineDesc"],
        "DictionaryFillDesc" => &["EntryFillDesc"],
        "EntryLineDesc" => &["LineDesc"],
        "EntryFillDesc" => &["FillDesc"],
        "LayerFeature" => &["Set"],
        "Set" => &[
            "Pad",
            "Hole",
            "SlotCavity",
            "Polyline",
            "Features",
            "GlobalFiducial",
            "LocalFiducial",
            "SpecRef",
            "NetShort",
            "NonstandardAttribute",
        ],
        "PadStackDef" => &["PadstackHoleDef", "PadstackPadDef"],
        "GlobalFiducial" | "LocalFiducial" => &[
            "Location",
            "Xform",
            "PinRef",
            "StandardPrimitiveRef",
            "Circle",
            "Oval",
            "RectCenter",
            "RectRound",
            "Contour",
        ],
        "PadstackPadDef" | "Pad" => &[
            "Location",
            "Xform",
            "PinRef",
            "StandardPrimitiveRef",
            "UserPrimitiveRef",
            "Circle",
            "Oval",
            "RectCenter",
            "RectRound",
            "Contour",
        ],
        "Features" => &[
            "Location",
            "Xform",
            "StandardPrimitiveRef",
            "UserPrimitiveRef",
            "Circle",
            "Oval",
            "RectCenter",
            "RectRound",
            "Contour",
            "Line",
            "Arc",
            "Polyline",
            "UserSpecial",
        ],
        "EntryStandard" | "EntryUser" | "UserSpecial" => &[
            "Circle",
            "Oval",
            "RectCenter",
            "RectRound",
            "RectCham",
            "RectCorner",
            "Contour",
            "Line",
            "Arc",
            "Polyline",
            "UserSpecial",
        ],
        "Circle" | "Oval" | "RectCenter" | "RectRound" | "RectCham" | "RectCorner" | "Line"
        | "Arc" => &["FillDesc", "FillDescRef", "LineDesc", "LineDescRef"],
        "Contour" => &["Polygon", "Cutout"],
        "Cutout" => &["Polygon", "PolyBegin", "PolyStepSegment", "PolyStepCurve"],
        "Polygon" | "Polyline" => &[
            "PolyBegin",
            "PolyStepSegment",
            "PolyStepCurve",
            "FillDesc",
            "FillDescRef",
            "LineDesc",
            "LineDescRef",
        ],
        "SlotCavity" => &["Location", "Xform", "Outline", "Oval"],
        "Outline" => &["Polygon", "LineDesc", "LineDescRef"],
        "NetShort" => &["NetRef", "LayerRef", "Location"],
        "Hole"
        | "PadstackHoleDef"
        | "Location"
        | "Xform"
        | "PinRef"
        | "StandardPrimitiveRef"
        | "UserPrimitiveRef"
        | "FillDesc"
        | "FillDescRef"
        | "LineDesc"
        | "LineDescRef"
        | "PolyBegin"
        | "PolyStepSegment"
        | "PolyStepCurve"
        | "SpecRef"
        | "NetRef"
        | "LayerRef"
        | "NonstandardAttribute" => &[],
        name => bail!("Cannot prove via isolation: unsupported geometry {name}"),
    };
    for &child in &children {
        ensure!(
            allowed.contains(&doc.name(child)),
            "Cannot prove via isolation: unsupported {} inside {}",
            doc.name(child),
            doc.name(node)
        );
        check_geometry(doc, child)?;
    }
    // These are single-shape containers. The general parser may accept only
    // the first of several children, which is not safe for an isolation proof.
    if matches!(
        doc.name(node),
        "EntryStandard"
            | "EntryUser"
            | "Pad"
            | "PadstackPadDef"
            | "GlobalFiducial"
            | "LocalFiducial"
    ) {
        let shapes = children
            .iter()
            .filter(|&&c| !matches!(doc.name(c), "Location" | "Xform" | "PinRef"))
            .count();
        ensure!(
            shapes <= 1,
            "Multiple shapes in {} prevent safe via cleanup",
            doc.name(node)
        );
    }
    if doc.name(node) == "Contour" {
        ensure!(
            children
                .iter()
                .filter(|&&c| doc.name(c) == "Polygon")
                .count()
                == 1,
            "Contour must contain exactly one Polygon"
        );
    }
    Ok(())
}

#[cfg(feature = "cli")]
pub fn execute(
    file: &std::path::Path,
    output: &std::path::Path,
    resolution: Resolution,
) -> Result<()> {
    let xml = crate::utils::file::load_ipc_file(file)?;
    let (updated, count) = remove_unused_via_lands(&xml, resolution)?;
    if output.as_os_str() == "-" {
        pcb_ui::write_stdout(|stdout| stdout.write_all(updated.as_bytes()))?;
    } else {
        crate::utils::file::save_ipc_file(output, &updated)?;
    }
    eprintln!(
        "Removed {count} isolated interior via land definitions; drills and endpoint lands preserved"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pad(x: f64) -> String {
        format!(
            r#"<Pad padstackDefRef="V"><Location x="{x}" y="3"/><StandardPrimitiveRef id="land"/></Pad>"#
        )
    }

    fn board(extra: &str) -> String {
        let layers = ["TOP", "I1", "I2", "BOTTOM"];
        let definitions = layers
            .iter()
            .map(|name| {
                format!(r#"<Layer name="{name}" layerFunction="SIGNAL" polarity="POSITIVE"/>"#)
            })
            .collect::<String>();
        let stackup = layers
            .iter()
            .enumerate()
            .map(|(i, name)| format!(r#"<StackupLayer layerOrGroupRef="{name}" sequence="{i}"/>"#))
            .collect::<String>();
        let features = layers
            .iter()
            .map(|name| {
                format!(
                    r#"<LayerFeature layerRef="{name}"><Set net="GND">{}{}</Set>{}</LayerFeature>"#,
                    pad(2.0),
                    pad(7.0),
                    if *name == "I1" { extra } else { "" }
                )
            })
            .collect::<String>();
        format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
<Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="board"/>
<DictionaryStandard units="MILLIMETER"><EntryStandard id="land"><Circle diameter="0.8"/></EntryStandard></DictionaryStandard></Content>
<Ecad name="test"><CadHeader units="MILLIMETER"/><CadData>{definitions}
<Layer name="DRILL" layerFunction="DRILL"><Span fromLayer="TOP" toLayer="BOTTOM"/></Layer>
<Stackup name="stack"><StackupGroup name="group">{stackup}</StackupGroup></Stackup>
<Step name="board" type="BOARD">
<PadStackDef name="V"><PadstackHoleDef name="via" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="0" y="0"/></PadStackDef>
{features}
<LayerFeature layerRef="DRILL"><Set geometry="V"><Hole name="v1" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="2" y="3"/><Hole name="v2" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="7" y="3"/></Set></LayerFeature>
</Step></CadData></Ecad></IPC-2581>"#
        )
    }

    fn trace(x: f64, y: f64, width: f64) -> String {
        let end = x + 1.0;
        format!(
            r#"<Set net="OTHER"><Features><Location x="0" y="0"/><Line startX="{x}" startY="{y}" endX="{end}" endY="{y}"><LineDesc lineWidth="{width}" lineEnd="ROUND"/></Line></Features></Set>"#
        )
    }

    fn run(xml: &str) -> (String, usize) {
        remove_unused_via_lands(xml, Resolution::default()).unwrap()
    }

    #[test]
    fn removes_only_isolated_inner_lands_and_is_idempotent() {
        let xml = board(&trace(7.0, 3.0, 0.2));
        let (updated, count) = run(&xml);
        assert_eq!(count, 3);
        let doc = Doc::parse(&updated).unwrap();
        for (layer, expected) in [("TOP", 2), ("I1", 1), ("I2", 0), ("BOTTOM", 2)] {
            let node = doc
                .find_all("LayerFeature")
                .into_iter()
                .find(|&n| doc.attr(n, "layerRef") == Some(layer))
                .unwrap();
            assert_eq!(
                doc.source(node).matches("<Pad ").count(),
                expected,
                "{layer}"
            );
        }
        assert!(updated.contains(&trace(7.0, 3.0, 0.2)));
        assert_eq!(doc.find_all("Hole").len(), 2);
        assert_eq!(doc.find_all("PadstackHoleDef").len(), 1);
        assert_eq!(run(&updated), (updated, 0));
    }

    #[test]
    fn preserves_tangency_near_contact_and_full_stroke_width() {
        for y in [3.0, 3.49, 3.5, 3.5000001] {
            assert_eq!(run(&board(&trace(7.0, y, 0.2))).1, 3, "y={y}");
        }
        assert_eq!(run(&board(&trace(7.0, 3.55, 0.2))).1, 4);
    }

    #[test]
    fn maps_pads_after_a_features_container_that_expands_to_multiple_entries() {
        let features = r#"<Features><Location x="0" y="0"/><Line startX="20" startY="3" endX="21" endY="3"><LineDesc lineWidth="0.2" lineEnd="ROUND"/></Line><Line startX="7" startY="3" endX="8" endY="3"><LineDesc lineWidth="0.2" lineEnd="ROUND"/></Line></Features>"#;
        let start = r#"<LayerFeature layerRef="I1"><Set net="GND">"#;
        let xml = board("").replace(start, &format!("{start}{features}"));
        let (updated, count) = run(&xml);
        assert_eq!(count, 3);
        assert!(updated.contains(features));
        let doc = Doc::parse(&updated).unwrap();
        let layer = doc
            .find_all("LayerFeature")
            .into_iter()
            .find(|&n| doc.attr(n, "layerRef") == Some("I1"))
            .unwrap();
        assert!(doc.source(layer).contains(&pad(7.0)));
        assert!(!doc.source(layer).contains(&pad(2.0)));
    }

    fn plane(hole: bool) -> String {
        let cutout = if hole {
            r#"<Cutout><Polygon><PolyBegin x="6.4" y="2.4"/><PolyStepSegment x="7.6" y="2.4"/><PolyStepSegment x="7.6" y="3.6"/><PolyStepSegment x="6.4" y="3.6"/><PolyStepSegment x="6.4" y="2.4"/></Polygon></Cutout>"#
        } else {
            ""
        };
        format!(
            r#"<Set net="GND"><Features><Location x="0" y="0"/><Contour><Polygon><PolyBegin x="5" y="1"/><PolyStepSegment x="9" y="1"/><PolyStepSegment x="9" y="5"/><PolyStepSegment x="5" y="5"/><PolyStepSegment x="5" y="1"/></Polygon>{cutout}</Contour></Features></Set>"#
        )
    }

    #[test]
    fn distinguishes_plane_containment_clearance_holes_and_thermal_spokes() {
        assert_eq!(run(&board(&plane(false))).1, 3);
        assert_eq!(run(&board(&plane(true))).1, 4);
        assert_eq!(run(&board(&(plane(true) + &trace(7.4, 3.0, 0.1)))).1, 3);
    }

    fn panel(xml: &str, nested: bool, touching: bool) -> String {
        let array = format!(
            r#"<Step name="array" type="PALLET"><StepRepeat stepRef="board" x="10" y="0" nx="2" ny="1" dx="20" dy="0"/>{}</Step>"#,
            if touching {
                format!(
                    r#"<LayerFeature layerRef="I1">{}</LayerFeature>"#,
                    trace(17.0, 3.0, 0.2)
                )
            } else {
                String::new()
            }
        );
        let fab = if nested {
            r#"<Step name="fab" type="PALLET"><StepRepeat stepRef="array" x="0" y="10" nx="1" ny="2" dx="0" dy="20"/></Step>"#
        } else {
            ""
        };
        xml.replace(
            r#"<StepRef name="board"/>"#,
            if nested {
                r#"<StepRef name="fab"/>"#
            } else {
                r#"<StepRef name="array"/>"#
            },
        )
        .replace("</CadData>", &format!("{array}{fab}</CadData>"))
    }

    #[test]
    fn shared_definitions_require_isolation_in_every_array_and_panel_copy() {
        for nested in [false, true] {
            assert_eq!(run(&panel(&board(""), nested, false)).1, 4);
            // Panel copper touches one copy of the x=7 land on I1 only.
            assert_eq!(run(&panel(&board(""), nested, true)).1, 3);
        }
    }

    #[test]
    fn keeps_ambiguous_component_and_blind_hole_lands() {
        let xml = board("");
        assert_eq!(
            run(&xml.replace("platingStatus=\"VIA\"", "platingStatus=\"PLATED\"")).1,
            0
        );
        assert_eq!(
            run(&xml.replace("toLayer=\"BOTTOM\"", "toLayer=\"I2\"")).1,
            0
        );
        let duplicate = board(&format!("<Set>{}</Set>", pad(7.0)));
        assert_eq!(run(&duplicate).1, 3);
    }

    #[test]
    fn preserves_lands_touching_another_barrel_without_a_land() {
        let xml = board("").replace(
            "</Step>",
            r#"<LayerFeature layerRef="DRILL"><Set geometry="other"><Hole name="other" diameter="0.3" platingStatus="VIA" plusTol="0" minusTol="0" x="7.5" y="3"/></Set></LayerFeature></Step>"#,
        );
        assert_eq!(run(&xml).1, 2);
    }

    #[test]
    fn physical_stackup_not_declaration_order_determines_endpoints() {
        let xml = board("");
        let top = r#"<Layer name="TOP" layerFunction="SIGNAL" polarity="POSITIVE"/>"#;
        let permuted = xml
            .replace(top, "")
            .replace("<Stackup name=", &format!("{top}<Stackup name="));
        assert_eq!(run(&permuted).1, 4);
    }

    #[test]
    fn fails_closed_on_ignored_geometry_missing_shapes_and_missing_stackup() {
        let unknown = board("<Set><FutureCopperShape/></Set>");
        assert!(remove_unused_via_lands(&unknown, Resolution::default()).is_err());
        let missing = board("").replace("id=\"land\"/>", "id=\"missing\"/>");
        assert!(remove_unused_via_lands(&missing, Resolution::default()).is_err());
        let xml = board("");
        let doc = Doc::parse(&xml).unwrap();
        let xml = doc
            .apply(vec![doc.delete(doc.find_all("Stackup")[0])])
            .unwrap();
        assert!(remove_unused_via_lands(&xml, Resolution::default()).is_err());
    }
}
