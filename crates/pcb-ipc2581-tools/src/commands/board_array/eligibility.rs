//! Analysis only: canonical board outline clearance, not a mouse-bite panel.
use anyhow::{Context, Result, bail};
use ipc2581::{
    Ipc2581, Symbol,
    types::{LayerFunction, UserPrimitive, UserShapeType, ecad::SetFeature},
};
use pcb_ir::{
    dialects::ipc::{ArtworkScope, LayoutStepKind, ProfileSet, profile_occurrences_for},
    geom::{
        BBox, ContourBuf, ContourSet, FillRule, PathCmd, PathOp, Resolution, Segment,
        attachment::{
            QueryTolerance,
            outline::{OutlineFootprint, OutlineObstacle, eligible_outline},
        },
    },
    import::ipc2581::{ImportedDesign, LayerId, import_design},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) struct Evidence {
    pub id: String,
    pub region: Option<ContourSet>,
}

pub(super) struct Prepared {
    pub report: Value,
    pub substrate: ContourSet,
    pub evidence: Vec<Evidence>,
    pub intervals: Vec<pcb_ir::geom::attachment::outline::OutlineInterval>,
}

struct CourtyardGroup {
    component: Option<ipc2581::Symbol>,
    contours: Vec<ContourBuf>,
    sources: Vec<String>,
    positive: bool,
}

/// Analyze one canonical board, including profile cutouts. Explicit exclusions
/// are already in canonical board coordinates, in mm. No coverage is inferred
/// for unexported keep-outs, and no manufacturing allowances are selected here.
/// Multiple board definitions and unresolved courtyard references are unsupported.
pub fn analyze(
    xml: &str,
    footprint: OutlineFootprint,
    clearance_mm: f64,
    exclusions: &[OutlineObstacle<'_>],
    resolution: Resolution,
) -> Result<Value> {
    Ok(prepare(xml, footprint, clearance_mm, exclusions, resolution)?.report)
}

pub(super) fn prepare(
    xml: &str,
    footprint: OutlineFootprint,
    clearance_mm: f64,
    exclusions: &[OutlineObstacle<'_>],
    resolution: Resolution,
) -> Result<Prepared> {
    if !clearance_mm.is_finite() || clearance_mm < 0.0 {
        bail!("clearance must be finite and nonnegative");
    }
    let ipc = Ipc2581::parse(xml).context("Failed to parse IPC-2581 input")?;
    validate_courtyard_references(&ipc)?;
    // Keep small substrate cutouts and courtyard regions; accuracy remains the
    // caller's existing geometry budget (--accuracy-um in the CLI).
    let resolution = resolution.strict();
    let imported = import_design(&ipc, resolution)?;
    let doc = &imported.geometry;
    // Both BoardOutlines and ArtworkScope::Board must select the same board.
    // Do not resolve mixed-board panels in this analysis-only phase.
    if doc
        .layout
        .steps
        .iter()
        .filter(|step| step.kind == LayoutStepKind::Board)
        .count()
        != 1
    {
        bail!("unsupported input: outline eligibility requires exactly one board definition");
    }
    let profiles = profile_occurrences_for(doc, ProfileSet::BoardOutlines);
    if profiles.is_empty() {
        bail!("unsupported input: canonical board has no substrate profile");
    }
    let mut substrate = ContourSet::empty(resolution);
    for occurrence in profiles {
        let mut region = ContourSet::from_contours(
            &doc.transformed_path_contours(occurrence.profile.outer_path, occurrence.transform),
            FillRule::EvenOdd,
            resolution,
        )?;
        for cutout in occurrence.profile.cutouts.slice(&doc.profile_cutouts) {
            let hole = ContourSet::from_contours(
                &doc.transformed_path_contours(cutout.path, occurrence.transform),
                FillRule::EvenOdd,
                resolution,
            )?;
            region = region.difference(&hole)?;
        }
        substrate = substrate.union(&region)?;
    }
    let (mut evidence, ignored_footprints) = courtyard_evidence(&imported, resolution)?;
    evidence.extend(exclusions.iter().map(|exclusion| Evidence {
        id: format!("explicit:{}", exclusion.id),
        region: exclusion.region.cloned(),
    }));
    for obstacle in &mut evidence {
        if let Some(region) = &mut obstacle.region {
            *region = region.disk_dilate(clearance_mm)?;
        }
    }
    let obstacles = evidence
        .iter()
        .map(|e| OutlineObstacle {
            id: &e.id,
            region: e.region.as_ref(),
        })
        .collect::<Vec<_>>();
    let tolerance = QueryTolerance {
        boundary_mm: 0.0,
        numerical_mm: pcb_ir::geom::tol::EPSILON_MM,
    };
    let intervals = eligible_outline(&substrate, &obstacles, footprint, tolerance)?;
    let report = json!({
        "phase": "outline-eligibility-only",
        "manufacturing_ready": false,
        "scope": "canonical-board",
        "source_xml_sha256": hex::encode(Sha256::digest(xml.as_bytes())),
        "units": "mm",
        "ignored_footprints": ignored_footprints,
        "policy": {
            "missing_courtyard": "ignore-footprint",
            "width_mm": footprint.width_mm, "inward_mm": footprint.inward_mm,
            "outward_mm": footprint.outward_mm, "clearance_mm": clearance_mm,
            "accuracy_mm": resolution.accuracy.max_error_mm(),
            "significance_mm": resolution.tolerance_mm,
            "boundary_mm": tolerance.boundary_mm, "numerical_mm": tolerance.numerical_mm,
        },
        "limitations": [
            "Eligible means clear only of supplied courtyard/exclusion evidence on the prepared polygon model; interval endpoints carry no guarantee.",
            "Closed courtyard contours are filled conservatively, regardless of outline ink styling. Both board sides are included without additional mirroring.",
            "Footprints without courtyard evidence contribute no obstruction and are listed in ignored_footprints. Clearance is conditional on supplied courtyards being complete; ignored physical components may overhang. Present but unusable courtyards are errors.",
            "General keep-out export coverage is not established. No copper, pad, drill, stackup or 3D collision checks are performed.",
            "No tabs, perforations, frame connections, router access, mechanics or panel export are generated."
        ],
        "diagnostics": doc.diagnostics.iter().map(|d| json!({"severity": format!("{:?}", d.severity), "message": d.message})).collect::<Vec<_>>(),
        "evidence": evidence.iter().map(|e| json!({"id": e.id, "available": e.region.as_ref().is_some_and(|r| !r.is_empty())})).collect::<Vec<_>>(),
        "intervals": intervals.iter().map(|i| json!({
            "ring": i.boundary.ring, "edge": i.edge,
            "start_mm": i.start_mm, "end_mm": i.end_mm,
            "start": [i.start.x, i.start.y], "end": [i.end.x, i.end.y],
            "state": format!("{:?}", i.state), "landing": format!("{:?}", i.landing),
            "obstacles": i.obstacles.iter().map(|&index| &evidence[index].id).collect::<Vec<_>>(),
            "uncertainty_mm": i.uncertainty_mm,
        })).collect::<Vec<_>>(),
    });
    Ok(Prepared {
        report,
        substrate,
        evidence,
        intervals,
    })
}

fn courtyard_evidence(
    imported: &ImportedDesign,
    resolution: Resolution,
) -> Result<(Vec<Evidence>, Vec<String>)> {
    let mut evidence = Vec::new();
    let mut covered = Vec::new();
    for (index, layer) in imported
        .layer_definitions
        .iter()
        .enumerate()
        .filter(|(_, layer)| layer.layer_function == LayerFunction::Courtyard)
    {
        let mut groups: Vec<CourtyardGroup> = Vec::new();
        for occurrence in
            imported.feature_occurrences(LayerId(index as u32), ArtworkScope::Board)?
        {
            let feature = imported.feature_definition(occurrence.id.feature).unwrap();
            if feature.source_layer_ref != Some(layer.name) {
                continue;
            }
            let component = feature
                .set
                .and_then(|set| imported.geometry.feature_sets[set as usize].component_ref);
            let contours = feature
                .paths
                .indices()
                .flat_map(|path| {
                    imported
                        .geometry
                        .transformed_path_contours(path, occurrence.root_from_local)
                })
                .collect::<Vec<_>>();
            let source = format!(
                "feature-{}:placement-{:?}",
                occurrence.id.feature.0, occurrence.id.placement
            );
            let positive = feature.polarity == pcb_ir::geom::Polarity::Dark && !contours.is_empty();
            if let Some(group) = groups
                .iter_mut()
                .find(|g| component.is_some() && g.component == component)
            {
                group.contours.extend(contours);
                group.sources.push(source);
                group.positive &= positive;
            } else {
                groups.push(CourtyardGroup {
                    component,
                    contours,
                    sources: vec![source],
                    positive,
                });
            }
        }
        for group in groups {
            let region = if group.positive {
                courtyard_region(&group.contours, resolution)?
            } else {
                None
            };
            if !region.as_ref().is_some_and(|r| !r.is_empty()) {
                bail!(
                    "unusable courtyard on {} for {} ({})",
                    imported.resolve(layer.name),
                    group
                        .component
                        .map(|c| imported.resolve(c))
                        .unwrap_or("unassociated"),
                    group.sources.join(",")
                );
            }
            covered.push(group.component);
            evidence.push(Evidence {
                id: format!(
                    "courtyard:{}:{}:{}",
                    imported.resolve(layer.name),
                    group
                        .component
                        .map(|c| imported.resolve(c))
                        .unwrap_or("unassociated"),
                    group.sources.join(",")
                ),
                region,
            });
        }
    }
    let mut ignored = Vec::new();
    for occurrence in imported.component_occurrences(ArtworkScope::Board)? {
        let component = imported
            .component_definition(occurrence.id.component)
            .unwrap();
        if component.source.ref_des.is_none() || !covered.contains(&component.source.ref_des) {
            ignored.push(format!(
                "{}:component-{}",
                component
                    .source
                    .ref_des
                    .map(|r| imported.resolve(r))
                    .unwrap_or("unnamed"),
                occurrence.id.component.0
            ));
        }
    }
    Ok((evidence, ignored))
}

// Import is intentionally permissive and may discard unresolved references,
// including nested shapes without diagnostics. Check source evidence first:
// a surviving sibling must not make an incomplete courtyard appear complete.
fn validate_courtyard_references(ipc: &Ipc2581) -> Result<()> {
    let cad = &ipc
        .ecad()
        .context("IPC-2581 file has no ECAD section")?
        .cad_data;
    let mut features = cad
        .steps
        .iter()
        .flat_map(|step| &step.layer_features)
        .filter(|features| {
            cad.layers.iter().any(|layer| {
                layer.name == features.layer_ref && layer.layer_function == LayerFunction::Courtyard
            })
        })
        .flat_map(|layer| &layer.sets)
        .flat_map(|set| &set.features)
        .collect::<Vec<_>>();
    while let Some(feature) = features.pop() {
        match feature {
            SetFeature::PlacementGroup(group) => features.extend(&group.features),
            SetFeature::StandardPrimitiveRef(reference) => {
                if !ipc
                    .content()
                    .dictionary_standard
                    .entries
                    .iter()
                    .any(|entry| entry.id == reference.id)
                {
                    bail!(
                        "unsupported input: missing courtyard standard primitive '{}'",
                        ipc.resolve(reference.id)
                    );
                }
            }
            SetFeature::UserPrimitiveRef(reference) => {
                let primitive = ipc
                    .content()
                    .dictionary_user
                    .entries
                    .iter()
                    .rev() // Match the importer's last-definition-wins map.
                    .find(|entry| entry.id == reference.id)
                    .with_context(|| {
                        format!(
                            "unsupported input: missing courtyard user primitive '{}'",
                            ipc.resolve(reference.id)
                        )
                    })?;
                validate_user_primitive(ipc, &primitive.primitive, &mut vec![reference.id])?;
            }
            SetFeature::UserPrimitive(feature) => {
                validate_user_primitive(ipc, &feature.primitive, &mut Vec::new())?
            }
            // Padstack selection has its own permissive reference resolution;
            // it is not a supported source of courtyard envelopes in this phase.
            SetFeature::Pad(_) => bail!("unsupported input: courtyard padstack evidence"),
            _ => {}
        }
    }
    Ok(())
}

fn validate_user_primitive(
    ipc: &Ipc2581,
    primitive: &UserPrimitive,
    ancestors: &mut Vec<Symbol>,
) -> Result<()> {
    let UserPrimitive::UserSpecial(special) = primitive;
    for shape in &special.shapes {
        if let UserShapeType::UserPrimitiveRef(id) = shape.shape {
            if ancestors.contains(&id) {
                bail!(
                    "unsupported input: cyclic courtyard user primitive '{}'",
                    ipc.resolve(id)
                );
            }
            let entry = ipc
                .content()
                .dictionary_user
                .entries
                .iter()
                .rev()
                .find(|entry| entry.id == id)
                .with_context(|| {
                    format!(
                        "unsupported input: missing courtyard user primitive '{}'",
                        ipc.resolve(id)
                    )
                })?;
            ancestors.push(id);
            validate_user_primitive(ipc, &entry.primitive, ancestors)?;
            ancestors.pop();
        }
    }
    Ok(())
}

// KiCad can export a single courtyard as separate line/arc features. Join only
// exact, unambiguous endpoints within one component/layer occurrence. No gap
// snapping, hull, or ink-width envelope substitutes for missing source evidence.
fn courtyard_region(contours: &[ContourBuf], resolution: Resolution) -> Result<Option<ContourSet>> {
    let mut loops = Vec::new();
    let mut segments = Vec::new();
    let uncertainty = contours
        .iter()
        .map(|c| c.uncertainty_mm)
        .fold(0.0, f64::max);
    for contour in contours {
        if contour.cmds.last().is_some_and(|c| c.op == PathOp::Close) {
            loops.push(contour.clone());
        } else {
            segments.extend(contour.segments());
        }
    }
    let mut open_components = Vec::new();
    while let Some(first) = segments.pop() {
        let mut component = vec![first];
        let mut index = 0;
        while index < component.len() {
            let endpoints = [component[index].start(), component[index].end()];
            let mut candidate = 0;
            while candidate < segments.len() {
                if endpoints.contains(&segments[candidate].start())
                    || endpoints.contains(&segments[candidate].end())
                {
                    component.push(segments.remove(candidate));
                } else {
                    candidate += 1;
                }
            }
            index += 1;
        }
        let closed = component.iter().all(|segment| {
            [segment.start(), segment.end()].iter().all(|point| {
                component
                    .iter()
                    .map(|other| {
                        usize::from(other.start() == *point) + usize::from(other.end() == *point)
                    })
                    .sum::<usize>()
                    == 2
            })
        });
        if closed {
            loops.push(closed_component(component, uncertainty)?);
        } else {
            open_components.push(component);
        }
    }
    if loops.is_empty() {
        return Ok(None);
    }
    let region = ContourSet::from_filled_contours(&loops, resolution)?;
    // An open component is harmless only when its entire conservative bounds
    // fit strictly inside the enclosure. Segment bboxes include curve extrema
    // (cubic control bounds are conservative); source and prepared-boundary
    // uncertainty plus the requested preparation error expand the proof
    // rectangle. This is not gap snapping: expansion can only reject evidence,
    // makes boundary contact fail, and gives axial lines a nonzero area that
    // survives the region representation.
    for component in open_components {
        let guard = uncertainty + region.uncertainty_mm + resolution.accuracy.max_error_mm();
        let bbox = component
            .iter()
            .fold(BBox::empty(), |bbox, segment| bbox.union(segment.bbox()))
            .expand(guard);
        if !bbox.is_valid()
            || bbox.width() <= 0.0
            || bbox.height() <= 0.0
            || !ContourSet::rectangle(bbox, resolution)
                .difference(&region)?
                .is_empty()
        {
            return Ok(None);
        }
    }
    Ok(Some(region))
}

fn closed_component(mut segments: Vec<Segment>, uncertainty: f64) -> Result<ContourBuf> {
    let first = segments.pop().context("empty closed courtyard component")?;
    let start = first.start();
    let mut end = first.end();
    let mut cmds = vec![PathCmd::move_to(start), segment_command(first, false)];
    while end != start {
        let index = segments
            .iter()
            .position(|segment| segment.start() == end || segment.end() == end)
            .context("disconnected closed courtyard component")?;
        let segment = segments.remove(index);
        let reverse = segment.end() == end;
        end = if reverse {
            segment.start()
        } else {
            segment.end()
        };
        cmds.push(segment_command(segment, reverse));
    }
    cmds.push(PathCmd::close());
    Ok(ContourBuf::new(cmds).with_uncertainty(uncertainty))
}

fn segment_command(segment: Segment, reverse: bool) -> PathCmd {
    let end = if reverse {
        segment.start()
    } else {
        segment.end()
    };
    match segment {
        Segment::Line { .. } => PathCmd::line_to(end),
        Segment::Arc(arc) => PathCmd::arc_to(end, arc.center, arc.clockwise ^ reverse),
        Segment::Ellipse(arc) => PathCmd::ellipse_to(
            end,
            arc.center,
            arc.x_axis,
            arc.y_axis,
            arc.clockwise ^ reverse,
        ),
        Segment::Cubic { c1, c2, .. } => {
            if reverse {
                PathCmd::cubic_to(c2, c1, end)
            } else {
                PathCmd::cubic_to(c1, c2, end)
            }
        }
    }
}

#[cfg(feature = "cli")]
pub fn execute(
    input: &std::path::Path,
    output: &std::path::Path,
    footprint: OutlineFootprint,
    clearance_mm: f64,
    resolution: Resolution,
) -> Result<()> {
    let xml = crate::utils::file::load_ipc_file(input)?;
    let report = analyze(&xml, footprint, clearance_mm, &[], resolution)?;
    let mut json = serde_json::to_vec_pretty(&report)?;
    json.push(b'\n');
    if output.as_os_str() == "-" {
        pcb_ui::write_stdout(|stdout| stdout.write_all(&json))?;
    } else {
        std::fs::write(output, json)?;
    }
    anstream::eprintln!("Outline eligibility analysis only; no mouse-bite panel generated.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::geom::Point;

    // Asymmetric local outline, translated/rotated top occurrence, and a
    // separately placed bottom occurrence. Component placement must not be
    // applied to already placed feature geometry (nor bottom mirrored again).
    fn fixture() -> String {
        let courtyard = r#"<Set componentRef="U1"><Features>
            <Xform rotation="90"/><Location x="8" y="1"/>
            <Polygon><PolyBegin x="0" y="0"/>
            <PolyStepSegment x="4" y="0"/><PolyStepSegment x="4" y="2"/>
            <PolyStepSegment x="0" y="2"/><PolyStepSegment x="0" y="0"/>
            <LineDesc lineWidth="0.05"/><FillDesc fillProperty="HOLLOW"/>
            </Polygon></Features></Set>"#;
        format!(
            r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
          <Content roleRef="owner"><FunctionMode mode="ASSEMBLY"/><StepRef name="board"/></Content>
          <Ecad><CadHeader units="MILLIMETER"/><CadData>
          <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
          <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM"/>
          <Layer name="F.Courtyard" layerFunction="COURTYARD" side="TOP"/>
          <Layer name="B.Courtyard" layerFunction="COURTYARD" side="BOTTOM"/>
          <Step name="board" type="BOARD"><Datum x="0" y="0"/>
          <Profile><Polygon><PolyBegin x="0" y="0"/><PolyStepSegment x="20" y="0"/>
          <PolyStepSegment x="20" y="10"/><PolyStepSegment x="0" y="10"/>
          <PolyStepSegment x="0" y="0"/></Polygon>
          <Cutout><Polygon><PolyBegin x="10" y="3"/><PolyStepSegment x="12" y="3"/>
          <PolyStepSegment x="12" y="5"/><PolyStepSegment x="10" y="5"/>
          <PolyStepSegment x="10" y="3"/></Polygon></Cutout></Profile>
          <Component refDes="U1" part="p" layerRef="TOP" mountType="SMT"><Location x="100" y="100"/></Component>
          <Component refDes="U2" part="p" layerRef="BOTTOM" mountType="SMT"><Xform mirror="true"/><Location x="200" y="100"/></Component>
          <LayerFeature layerRef="F.Courtyard">{courtyard}</LayerFeature>
          <LayerFeature layerRef="B.Courtyard">{bottom}</LayerFeature>
          </Step></CadData></Ecad></IPC-2581>"#,
            bottom = courtyard
                .replace("U1", "U2")
                .replace("x=\"8\" y=\"1\"", "x=\"23\" y=\"6\"")
                .replace("rotation=\"90\"", "rotation=\"0\" mirror=\"true\"")
        )
    }

    fn footprint() -> OutlineFootprint {
        OutlineFootprint {
            width_mm: 1.0,
            inward_mm: 0.2,
            outward_mm: 1.0,
        }
    }

    #[test]
    fn surviving_courtyard_does_not_hide_unresolved_source_references() {
        for (feature, dictionary) in [
            ("<UserPrimitiveRef id=\"missing\"/>", ""),
            ("<StandardPrimitiveRef id=\"missing\"/>", ""),
            (
                "<UserPrimitiveRef id=\"outer\"/>",
                r#"<DictionaryUser units="MILLIMETER"><EntryUser id="outer"><UserSpecial><Circle diameter="1"/><UserPrimitiveRef id="missing"/></UserSpecial></EntryUser></DictionaryUser>"#,
            ),
            (
                "<UserPrimitiveRef id=\"outer\"/>",
                r#"<DictionaryUser units="MILLIMETER"><EntryUser id="outer"><UserSpecial><Circle diameter="1"/></UserSpecial></EntryUser><EntryUser id="outer"><UserSpecial><UserPrimitiveRef id="missing"/></UserSpecial></EntryUser></DictionaryUser>"#,
            ),
        ] {
            let xml = fixture()
                .replace("</Content>", &format!("{dictionary}</Content>"))
                .replacen("</LayerFeature>", &format!("<Set componentRef=\"U1\"><Features><Location x=\"19\" y=\"1\"/>{feature}</Features></Set></LayerFeature>"), 1);
            let error = analyze(&xml, footprint(), 0.0, &[], Resolution::default()).unwrap_err();
            assert!(error.to_string().contains("missing"), "{error:#}");
        }
    }

    #[test]
    fn shared_nested_primitives_are_valid_but_cycles_are_unsupported() {
        let dictionary = r#"<DictionaryUser units="MILLIMETER">
          <EntryUser id="outer"><UserSpecial><UserPrimitiveRef id="inner"/><UserPrimitiveRef id="inner"/></UserSpecial></EntryUser>
          <EntryUser id="inner"><UserSpecial><Circle diameter="1"/></UserSpecial></EntryUser>
          </DictionaryUser>"#;
        let xml = fixture().replace("</Content>", &format!("{dictionary}</Content>"))
            .replacen("</LayerFeature>", r#"<Set componentRef="U1"><Features><Location x="8" y="3"/><UserPrimitiveRef id="outer"/></Features></Set></LayerFeature>"#, 1);
        let report = analyze(&xml, footprint(), 0.0, &[], Resolution::default()).unwrap();
        assert!(
            report["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .all(|e| e["available"] == true)
        );
        let cyclic = xml.replace(
            "<Circle diameter=\"1\"/>",
            "<UserPrimitiveRef id=\"outer\"/>",
        );
        let error = analyze(&cyclic, footprint(), 0.0, &[], Resolution::default()).unwrap_err();
        assert!(error.to_string().contains("cyclic"), "{error:#}");
    }

    #[test]
    fn zero_count_board_repeat_cannot_mix_substrate_and_evidence() {
        let xml = fixture().replace("<StepRef name=\"board\"/>", "<StepRef name=\"panel\"/>")
            .replace("<Step name=\"board\"", r#"<Step name="panel" type="PALLET"><StepRepeat stepRef="unused" nx="0" ny="1"/><StepRepeat stepRef="board" nx="1" ny="1"/></Step>
              <Step name="unused" type="BOARD"><Profile><Polygon><PolyBegin x="100" y="100"/><PolyStepSegment x="120" y="100"/><PolyStepSegment x="120" y="110"/><PolyStepSegment x="100" y="110"/><PolyStepSegment x="100" y="100"/></Polygon></Profile></Step>
              <Step name="board""#);
        let error = analyze(&xml, footprint(), 0.0, &[], Resolution::default()).unwrap_err();
        assert!(
            error.to_string().contains("one board definition"),
            "{error:#}"
        );
    }

    #[test]
    fn fragmented_courtyard_joins_by_component_but_gaps_are_errors() {
        let xml = fixture();
        let start = xml.find("<LayerFeature layerRef=\"F.Courtyard\">").unwrap();
        let end = start + xml[start..].find("</LayerFeature>").unwrap();
        let lines = [(0, 0, 4, 0), (4, 2, 4, 0), (0, 2, 4, 2), (0, 2, 0, 0)];
        let sets = lines.iter().map(|(x1, y1, x2, y2)| format!(r#"<Set componentRef="U1"><Features><Xform rotation="90"/><Location x="8" y="1"/><Line startX="{x1}" startY="{y1}" endX="{x2}" endY="{y2}"><LineDesc lineWidth="0.05"/></Line></Features></Set>"#)).collect::<Vec<_>>();
        for count in [4, 3] {
            let mut xml = xml.clone();
            xml.replace_range(
                start..end,
                &format!(
                    "<LayerFeature layerRef=\"F.Courtyard\">{}",
                    sets[..count].join("")
                ),
            );
            let imported =
                import_design(&Ipc2581::parse(&xml).unwrap(), Resolution::default()).unwrap();
            let result = courtyard_evidence(&imported, Resolution::default());
            if count == 3 {
                assert!(
                    result
                        .err()
                        .unwrap()
                        .to_string()
                        .contains("unusable courtyard")
                );
                continue;
            }
            let (evidence, _) = result.unwrap();
            let top = &evidence[0];
            assert!(
                top.region
                    .as_ref()
                    .unwrap()
                    .prepare_query()
                    .signed_distance(Point::new(7.0, 3.0))
                    .unwrap()
                    .mm
                    < -0.9
            );
            assert_eq!(top.id.matches("feature-").count(), 4);
        }
    }

    fn fragmented_rectangle_with(extra: Vec<ContourBuf>) -> Vec<ContourBuf> {
        let point = |x, y| Point::new(x, y);
        let mut contours = [
            (point(0.0, 0.0), point(10.0, 0.0)),
            (point(10.0, 0.0), point(10.0, 6.0)),
            (point(10.0, 6.0), point(0.0, 6.0)),
            (point(0.0, 6.0), point(0.0, 0.0)),
        ]
        .into_iter()
        .map(|(start, end)| ContourBuf::new(vec![PathCmd::move_to(start), PathCmd::line_to(end)]))
        .collect::<Vec<_>>();
        contours.extend(extra);
        contours
    }

    fn open_line(start: Point, end: Point) -> ContourBuf {
        ContourBuf::new(vec![PathCmd::move_to(start), PathCmd::line_to(end)])
    }

    #[test]
    fn fragmented_enclosure_allows_only_fully_enclosed_open_components() {
        let center_marks = vec![
            open_line(Point::new(4.0, 2.0), Point::new(6.0, 2.0)),
            open_line(Point::new(5.0, 1.0), Point::new(5.0, 3.0)),
        ];
        let expected = courtyard_region(
            &fragmented_rectangle_with(Vec::new()),
            Resolution::default(),
        )
        .unwrap()
        .unwrap();
        let accepted = courtyard_region(
            &fragmented_rectangle_with(center_marks),
            Resolution::default(),
        )
        .unwrap()
        .unwrap();
        assert!(accepted.difference(&expected).unwrap().is_empty());
        assert!(expected.difference(&accepted).unwrap().is_empty());

        for y in [6.0, 7.0] {
            let outside = open_line(Point::new(4.0, y), Point::new(6.0, y));
            assert!(
                courtyard_region(
                    &fragmented_rectangle_with(vec![outside]),
                    Resolution::default()
                )
                .unwrap()
                .is_none()
            );
        }
    }

    #[test]
    fn open_only_and_curves_bulging_outside_are_rejected() {
        assert!(
            courtyard_region(
                &[open_line(Point::new(1.0, 1.0), Point::new(2.0, 1.0))],
                Resolution::default()
            )
            .unwrap()
            .is_none()
        );

        // Both endpoints are inside, but the upper semicircle reaches y=7.
        let arc = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(1.0, 3.0)),
            PathCmd::arc_to(Point::new(9.0, 3.0), Point::new(5.0, 3.0), false),
        ]);
        assert!(
            courtyard_region(&fragmented_rectangle_with(vec![arc]), Resolution::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reversed_curve_join_preserves_the_enclosed_side_and_uncertainty() {
        let arc = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(2.0, 0.0)),
            PathCmd::arc_to(Point::new(-2.0, 0.0), Point::new(0.0, 0.0), false),
        ])
        .with_uncertainty(0.001);
        // Joining starts with this line and must reverse the arc.
        let line = ContourBuf::new(vec![
            PathCmd::move_to(Point::new(2.0, 0.0)),
            PathCmd::line_to(Point::new(-2.0, 0.0)),
        ]);
        let region = courtyard_region(&[arc, line], Resolution::default())
            .unwrap()
            .unwrap();
        assert!(
            region
                .prepare_query()
                .signed_distance(Point::new(0.0, 1.0))
                .unwrap()
                .mm
                < -0.9
        );
        assert!(
            region
                .prepare_query()
                .signed_distance(Point::new(0.0, -1.0))
                .unwrap()
                .mm
                > 0.9
        );
        assert!(region.uncertainty_mm >= 0.001);
    }

    #[test]
    fn courtyard_occurrences_fill_hollow_envelopes_on_both_sides() {
        let ipc = Ipc2581::parse(&fixture()).unwrap();
        let imported = import_design(&ipc, Resolution::default()).unwrap();
        let (evidence, ignored) = courtyard_evidence(&imported, Resolution::default()).unwrap();
        assert!(ignored.is_empty());
        assert_eq!(
            evidence.len(),
            2,
            "both components have applicable evidence"
        );
        let top = evidence[0].region.as_ref().unwrap();
        let bottom = evidence[1].region.as_ref().unwrap();
        assert!(
            top.prepare_query()
                .signed_distance(Point::new(7.0, 3.0))
                .unwrap()
                .mm
                < -0.9
        );
        assert!(
            bottom
                .prepare_query()
                .signed_distance(Point::new(21.0, 7.0))
                .unwrap()
                .mm
                < -0.9
        );
        assert!(
            top.prepare_query()
                .signed_distance(Point::new(3.0, 7.0))
                .unwrap()
                .mm
                > 0.0
        );
        let report = analyze(&fixture(), footprint(), 0.0, &[], Resolution::default()).unwrap();
        assert_eq!(report["manufacturing_ready"], false);
        assert!(
            report["intervals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["state"] == "Eligible")
        );
        assert!(
            report["intervals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["ring"] == 1)
        );
        assert!(report["intervals"].as_array().unwrap().iter().any(|i| {
            i["state"] == "Blocked"
                && i["obstacles"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|id| id.as_str().unwrap().contains("B.Courtyard:U2"))
        }));
        // At y=5.25 on the right edge, width alone cannot reach the bottom
        // courtyard beginning at y=6; an explicit 0.5 mm expansion can.
        for (clearance, expected) in [(0.0, "Eligible"), (0.5, "Blocked")] {
            let report = analyze(
                &fixture(),
                footprint(),
                clearance,
                &[],
                Resolution::default(),
            )
            .unwrap();
            let interval = report["intervals"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| {
                    i["start"][0] == 20.0
                        && i["end"][0] == 20.0
                        && i["start"][1]
                            .as_f64()
                            .unwrap()
                            .min(i["end"][1].as_f64().unwrap())
                            < 5.25
                        && i["start"][1]
                            .as_f64()
                            .unwrap()
                            .max(i["end"][1].as_f64().unwrap())
                            > 5.25
                })
                .unwrap();
            assert_eq!(interval["state"], expected);
        }
    }

    #[test]
    fn absent_courtyard_is_disclosed_without_poisoning_supplied_evidence() {
        let xml = fixture().replace(
            "<Component refDes=\"U2\"",
            "<Component refDes=\"NO_COURTYARD\"",
        );
        let report = analyze(&xml, footprint(), 0.0, &[], Resolution::default()).unwrap();
        let original = analyze(&fixture(), footprint(), 0.0, &[], Resolution::default()).unwrap();
        assert_eq!(report["intervals"], original["intervals"]);
        assert_eq!(
            report["ignored_footprints"],
            json!(["NO_COURTYARD:component-1"])
        );
        assert_eq!(report["policy"]["missing_courtyard"], "ignore-footprint");
        assert!(
            report["intervals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["state"] == "Eligible")
        );
        assert!(
            report["intervals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["state"] == "Blocked")
        );
    }

    #[test]
    fn explicit_missing_exclusion_and_invalid_policy_are_not_clearance() {
        let xml = fixture();
        let report = analyze(
            &xml,
            footprint(),
            0.0,
            &[OutlineObstacle {
                id: "connector-overhang",
                region: None,
            }],
            Resolution::default(),
        )
        .unwrap();
        assert!(
            !report["intervals"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["state"] == "Eligible")
        );
        assert!(analyze(&xml, footprint(), -0.1, &[], Resolution::default()).is_err());
        let invalid = OutlineFootprint {
            width_mm: 0.0,
            ..footprint()
        };
        assert!(analyze(&xml, invalid, 0.0, &[], Resolution::default()).is_err());
    }
}
