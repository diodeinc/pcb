//! Per-layer extraction of a step's set features into the geometry document.

use super::*;

/// Lookups shared by everything one import lowers: the file's dictionaries,
/// plus the padstacks of the step being extracted.
pub(super) struct ExtractContext<'a> {
    pub(super) strings: &'a Interner,
    pub(super) resolution: Resolution,
    pub(super) padstacks: HashMap<Symbol, &'a ipc2581::types::PadStackDef>,
    pub(super) line_descs: HashMap<Symbol, ipc2581::types::LineDesc>,
    pub(super) fill_descs: HashMap<Symbol, ipc2581::types::FillDesc>,
    pub(super) standard_primitives: HashMap<Symbol, &'a StandardPrimitive>,
    pub(super) user_primitives: HashMap<Symbol, &'a UserPrimitive>,
    /// Physical layer order for slot spans; `None` when the stackup gives
    /// none, so a spanned slot reaches only the layers its span names.
    pub(super) slot_layer_order: Option<Vec<Symbol>>,
}

impl<'a> ExtractContext<'a> {
    pub(super) fn new(
        strings: &'a Interner,
        content: &'a ipc2581::types::Content,
        resolution: Resolution,
    ) -> Self {
        Self {
            strings,
            resolution,
            padstacks: HashMap::new(),
            line_descs: content
                .dictionary_line_desc
                .entries
                .iter()
                .map(|entry| (entry.id, entry.line_desc))
                .collect(),
            fill_descs: content
                .dictionary_fill_desc
                .entries
                .iter()
                .map(|entry| (entry.id, entry.fill_desc))
                .collect(),
            standard_primitives: content
                .dictionary_standard
                .entries
                .iter()
                .map(|entry| (entry.id, &entry.primitive))
                .collect(),
            user_primitives: content
                .dictionary_user
                .entries
                .iter()
                .map(|entry| (entry.id, &entry.primitive))
                .collect(),
            slot_layer_order: None,
        }
    }

    /// The context for extracting layer features: slot spans need the
    /// physical layer order, which is resolved (and reported) once.
    pub(super) fn for_layers(
        ipc: &'a Ipc2581,
        resolution: Resolution,
        doc: &mut GeometryDocument,
    ) -> Self {
        let mut context = Self::new(ipc.interner(), ipc.content(), resolution);
        if let Some(ecad) = ipc.ecad() {
            context.slot_layer_order = resolve_slot_layer_order(doc, &ecad.cad_data);
        }
        context
    }

    pub(super) fn enter_step(&mut self, step: &'a Step) {
        self.padstacks = step
            .padstack_defs
            .iter()
            .map(|padstack| (padstack.name, padstack))
            .collect();
    }
}

pub(super) fn push_feature_set_record(
    doc: &mut GeometryDocument,
    layer: u32,
    source_set_index: u32,
    layer_feature: &ipc2581::types::LayerFeature,
    set: &ipc2581::types::FeatureSet,
    polarity: GeometryPolarity,
    copper_balance: Option<CopperBalanceMetadata>,
) -> u32 {
    let spec_refs = push_spec_refs(doc, set.spec_refs.slice(&layer_feature.spec_refs));
    let set_id = doc.feature_sets.len() as u32;
    doc.feature_sets.push(FeatureSet {
        layer,
        source_set_index,
        source_geometry_ref: set.geometry,
        component_ref: set.component_ref,
        net: set.net,
        polarity,
        copper_balance: copper_balance.is_some(),
        copper_balance_void: copper_balance.and_then(|metadata| {
            metadata.void.map(|void| CopperBalanceVoid {
                lattice: crate::geom::copper_balance::DenseCopperLattice {
                    origin: void.lattice_origin,
                    pitch_mm: void.lattice_pitch_mm,
                },
                radius_mm: void.radius_mm,
            })
        }),
        spec_refs,
        features: Span::new(doc.features.len() as u32, 0),
        bbox: BBox::empty(),
    });
    set_id
}

/// Append a lowered feature to its set, stamped with the step and source
/// layer it came from.
pub(super) fn push_extracted_feature(
    doc: &mut GeometryDocument,
    set_id: u32,
    step: &Step,
    source_layer: &Layer,
    mut feature: GeometryFeature,
    layer_bbox: &mut BBox,
) {
    feature.source_step_ref = Some(step.name);
    feature.source_step_kind = layout_step_kind(step);
    feature.source_layer_ref = Some(source_layer.name);
    complete_feature_intent(source_layer, &mut feature);
    *layer_bbox = layer_bbox.union(feature.bbox);
    doc.push_feature(set_id, feature);
}

/// One step's features on one layer, as a document of their own.
pub fn extract_step_layer_local(
    ipc: &Ipc2581,
    step: &Step,
    layers: &[Layer],
    layer: &Layer,
    layer_name: &str,
    resolution: Resolution,
) -> Result<GeometryDocument> {
    let mut doc = GeometryDocument::new();
    let mut context = ExtractContext::for_layers(ipc, resolution, &mut doc);
    context.enter_step(step);
    append_step_layer(&context, &mut doc, step, layers, layer, layer_name)?;
    Ok(doc)
}

/// Append `step`'s features on `layer` to `doc` as a new layer and return its
/// index. A step with nothing on the layer appends nothing but the warnings
/// for what it had to drop.
pub(super) fn append_step_layer(
    context: &ExtractContext<'_>,
    doc: &mut GeometryDocument,
    step: &Step,
    layers: &[Layer],
    layer: &Layer,
    layer_name: &str,
) -> Result<Option<u32>> {
    let mark = DocumentMark::of(doc);
    let feature_start = doc.features.len() as u32;
    let set_start = doc.feature_sets.len() as u32;
    let spec_refs = push_spec_refs(doc, &layer.spec_refs);
    let layer_index = doc.layers.len() as u32;
    doc.layers.push(GeometryLayer {
        name: layer_name.to_string(),
        source_layer_ref: layer.name,
        layer_function: layer.layer_function,
        spec_refs,
        sets: Span::new(set_start, 0),
        features: Span::new(feature_start, 0),
        bbox: BBox::empty(),
    });

    let mut layer_bbox = BBox::empty();
    let layer_polarity = map_polarity(layer.polarity.unwrap_or(Polarity::Positive));
    if layer_polarity == GeometryPolarity::Clear {
        push_negative_layer_plane(doc, layer_index, step, layer, &mut layer_bbox);
    }

    for layer_feature in step
        .layer_features
        .iter()
        .filter(|feature| feature.layer_ref == layer.name)
    {
        for (set_index, set) in layer_feature.sets.iter().enumerate() {
            // A Set's own polarity is absolute and an unmarked Set inherits the
            // layer's. They do not compose: Allegro marks anti-etch on NEGATIVE
            // planes as NEGATIVE sets, which clear like the antipads beside them.
            let polarity = set.polarity.map(map_polarity).unwrap_or(layer_polarity);
            let copper_balance = set_copper_balance_metadata(
                context.strings,
                set.nonstandard_attributes
                    .slice(&layer_feature.nonstandard_attributes),
            )?;
            if copper_balance.is_some_and(|metadata| metadata.void.is_some())
                && set.features.len() != 1
            {
                bail!("copper-balance full_void set must contain exactly one feature group");
            }
            let set_id = push_feature_set_record(
                doc,
                layer_index,
                set_index as u32,
                layer_feature,
                set,
                polarity,
                copper_balance,
            );

            let set_features = set.features.slice(&layer_feature.features);
            for (feature_index, set_feature) in set_features.iter().enumerate() {
                let source = SourceRef {
                    set_index: set_index as u32,
                    feature_index: feature_index as u32,
                    definition: None,
                };
                let mark = DocumentMark::of(doc);
                let features = extract_set_feature(
                    context,
                    layer.name,
                    set.net,
                    polarity,
                    source,
                    set_feature,
                    doc,
                )?;
                let features = keep_finite(doc, mark, features, source);
                validate_copper_balance_structure(
                    copper_balance,
                    set_feature,
                    &features,
                    doc,
                    context.resolution,
                )?;

                for feature in features {
                    push_extracted_feature(doc, set_id, step, layer, feature, &mut layer_bbox);
                }
            }
        }
    }

    // Holes stay on their drill layer; slots also image on the copper
    // layers their fabrication layer spans.
    for layer_feature in &step.layer_features {
        let Some(source_layer) = layers.iter().find(|candidate| {
            candidate.name == layer_feature.layer_ref && candidate.layer_function.is_fabrication()
        }) else {
            continue;
        };
        let holes_image_here =
            source_layer.layer_function == LayerFunction::Drill && source_layer.name == layer.name;

        for (set_index, set) in layer_feature.sets.iter().enumerate() {
            let polarity = set.polarity.map(map_polarity).unwrap_or(layer_polarity);
            let copper_balance = set_copper_balance_metadata(
                context.strings,
                set.nonstandard_attributes
                    .slice(&layer_feature.nonstandard_attributes),
            )?;
            let mut emitted = Vec::new();
            let set_features = set.features.slice(&layer_feature.features);
            let holes = set_features
                .iter()
                .enumerate()
                .filter(|(_, feature)| holes_image_here && matches!(feature, SetFeature::Hole(_)));
            let slots = set_features.iter().enumerate().filter(|(_, feature)| {
                matches!(feature, SetFeature::Slot(slot) if slot_applies_to_layer(
                    source_layer,
                    layer,
                    context.slot_layer_order.as_deref(),
                    slot,
                ))
            });
            for (feature_index, set_feature) in holes.chain(slots) {
                let source = SourceRef {
                    set_index: set_index as u32,
                    feature_index: feature_index as u32,
                    definition: None,
                };
                let mark = DocumentMark::of(doc);
                let feature = match set_feature {
                    SetFeature::Hole(hole) => extract_hole(source, set.geometry, hole, doc),
                    SetFeature::Slot(slot) => {
                        extract_slot(context, source, set.geometry, slot, doc)?
                    }
                    _ => continue,
                };
                emitted.extend(keep_finite(doc, mark, vec![feature], source));
            }

            if !emitted.is_empty() {
                let set_id = push_feature_set_record(
                    doc,
                    layer_index,
                    set_index as u32,
                    layer_feature,
                    set,
                    polarity,
                    copper_balance,
                );
                for feature in emitted {
                    push_extracted_feature(
                        doc,
                        set_id,
                        step,
                        source_layer,
                        feature,
                        &mut layer_bbox,
                    );
                }
            }
        }
    }

    if doc.features.len() as u32 == feature_start {
        mark.truncate(doc);
        doc.feature_sets.truncate(set_start as usize);
        doc.layers.truncate(layer_index as usize);
        return Ok(None);
    }
    let layer = &mut doc.layers[layer_index as usize];
    layer.features.count = doc.features.len() as u32 - feature_start;
    layer.sets.count = doc.feature_sets.len() as u32 - set_start;
    layer.bbox = layer_bbox;
    Ok(Some(layer_index))
}

/// Document lengths before one source feature was lowered, so everything it
/// pushed can be taken back.
#[derive(Debug, Clone, Copy)]
pub(super) struct DocumentMark {
    paths: usize,
    contours: usize,
    cmds: usize,
    placements: usize,
    placement_groups: usize,
    pin_refs: usize,
    spec_refs: usize,
}

impl DocumentMark {
    pub(super) fn of(doc: &GeometryDocument) -> Self {
        Self {
            paths: doc.arena.paths.len(),
            contours: doc.arena.contours.len(),
            cmds: doc.arena.cmds.len(),
            placements: doc.feature_placements.len(),
            placement_groups: doc.feature_placement_groups.len(),
            pin_refs: doc.pin_refs.len(),
            spec_refs: doc.spec_refs.len(),
        }
    }

    /// Whether every number pushed since the mark is finite. Path bounds
    /// cover stroke widths, which expand them.
    pub(super) fn pushed_is_finite(self, doc: &GeometryDocument) -> bool {
        doc.arena.cmds[self.cmds..]
            .iter()
            .all(|cmd| cmd.is_finite())
            && doc.arena.paths[self.paths..]
                .iter()
                .all(|path| path.bbox.is_valid())
            && doc.feature_placements[self.placements..]
                .iter()
                .all(|placement| affine_is_finite(*placement))
    }

    pub(super) fn truncate(self, doc: &mut GeometryDocument) {
        doc.arena.paths.truncate(self.paths);
        doc.arena.contours.truncate(self.contours);
        doc.arena.cmds.truncate(self.cmds);
        doc.feature_placements.truncate(self.placements);
        doc.feature_placement_groups.truncate(self.placement_groups);
        doc.pin_refs.truncate(self.pin_refs);
        doc.spec_refs.truncate(self.spec_refs);
    }
}

pub(super) fn affine_is_finite(transform: Affine2) -> bool {
    [
        transform.m00,
        transform.m01,
        transform.m02,
        transform.m10,
        transform.m11,
        transform.m12,
    ]
    .iter()
    .all(|value| value.is_finite())
}

/// The import boundary for non-finite numbers: a source feature whose lowered
/// geometry carries a NaN or an infinity is dropped whole and reported, so
/// none reaches the arena or poisons the bounds built on it.
pub(super) fn keep_finite(
    doc: &mut GeometryDocument,
    mark: DocumentMark,
    features: Vec<GeometryFeature>,
    source: SourceRef,
) -> Vec<GeometryFeature> {
    let finite = mark.pushed_is_finite(doc)
        && features.iter().all(|feature| {
            feature.bbox.is_valid()
                && feature.center.is_finite()
                && affine_is_finite(feature.transform)
        });
    if finite {
        return features;
    }
    mark.truncate(doc);
    doc.warn(format!(
        "Dropping feature {} of set {} because its geometry is not finite",
        source.feature_index, source.set_index
    ));
    Vec::new()
}

/// A NEGATIVE layer images what is removed from a plane filling the step
/// profile. The plane is the layer's first dark feature, so every consumer
/// composes the antipads against real material.
pub(super) fn push_negative_layer_plane(
    doc: &mut GeometryDocument,
    layer_index: u32,
    step: &Step,
    layer: &Layer,
    layer_bbox: &mut BBox,
) {
    let mut source_sets = step
        .layer_features
        .iter()
        .filter(|feature| feature.layer_ref == layer.name)
        .map(|feature| feature.sets.len() as u32);
    let Some(profile) = &step.profile else {
        if source_sets.next().is_some() {
            doc.warn(format!(
                "NEGATIVE layer '{}' clears nothing because its Step has no Profile to fill",
                doc.layers[layer_index as usize].name
            ));
        }
        return;
    };

    // One past the source sets, so set-scoped voids never reach the plane.
    let source_set_index = source_sets.max().unwrap_or(0);
    let set_id = doc.feature_sets.len() as u32;
    doc.feature_sets.push(FeatureSet {
        layer: layer_index,
        source_set_index,
        source_geometry_ref: None,
        component_ref: None,
        net: None,
        polarity: GeometryPolarity::Dark,
        copper_balance: false,
        copper_balance_void: None,
        spec_refs: Span::EMPTY,
        features: Span::new(doc.features.len() as u32, 0),
        bbox: BBox::empty(),
    });

    let mark = DocumentMark::of(doc);
    let path = push_outline_path(doc, &profile.polygon, &profile.cutouts, Affine2::IDENTITY);
    let mut feature = GeometryFeature::new(FeatureKind::Polygon, GeometryPolarity::Dark);
    feature.source.set_index = source_set_index;
    feature.bbox = doc.arena.paths[path as usize].bbox;
    feature.paths = Span::single(path);
    let source = feature.source;
    for feature in keep_finite(doc, mark, vec![feature], source) {
        push_extracted_feature(doc, set_id, step, layer, feature, layer_bbox);
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn extract_set_feature(
    context: &ExtractContext<'_>,
    layer_ref: Symbol,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    set_feature: &SetFeature,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    match set_feature {
        SetFeature::Pad(pad) => Ok(extract_pad(
            context, layer_ref, net, polarity, source, pad, doc,
        )?
        .into_iter()
        .collect()),
        SetFeature::Fiducial(fiducial) => Ok(extract_fiducial(
            context, net, polarity, source, fiducial, doc,
        )?
        .into_iter()
        .collect()),
        SetFeature::Stroke(stroke) => {
            Ok(extract_stroke(context, net, polarity, source, stroke, doc)
                .into_iter()
                .collect())
        }
        SetFeature::UserPrimitive(primitive) => {
            extract_inline_user_primitive(context, net, polarity, source, primitive, doc)
        }
        SetFeature::Polygon(polygon) => {
            Ok(vec![extract_polygon(net, polarity, source, polygon, doc)])
        }
        SetFeature::StandardPrimitiveRef(primitive_ref) => extract_feature_primitive(
            context,
            net,
            polarity,
            source,
            feature_location_transform(primitive_ref.x, primitive_ref.y),
            &FeatureShape::StandardPrimitiveRef(primitive_ref.id),
            doc,
        ),
        SetFeature::UserPrimitiveRef(primitive_ref) => extract_feature_primitive(
            context,
            net,
            polarity,
            source,
            feature_location_transform(primitive_ref.x, primitive_ref.y),
            &FeatureShape::UserPrimitiveRef(primitive_ref.id),
            doc,
        ),
        SetFeature::PlacementGroup(group) => {
            extract_feature_placement_group(context, layer_ref, net, polarity, source, group, doc)
        }
        SetFeature::Hole(_) | SetFeature::Slot(_) => Ok(Vec::new()),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn extract_feature_placement_group(
    context: &ExtractContext<'_>,
    layer_ref: Symbol,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    group: &ipc2581::types::ecad::FeaturePlacementGroup,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    let placement_start = doc.feature_placements.len() as u32;
    doc.feature_placements.extend(
        group.locations.iter().map(|location| {
            ipc_placement(Point::new(location.x, location.y), group.xform).transform
        }),
    );
    let placements = Span::new(
        placement_start,
        doc.feature_placements.len() as u32 - placement_start,
    );
    let group_id = doc.feature_placement_groups.len() as u32;
    doc.feature_placement_groups.push(FeaturePlacementGroup {
        placements,
        features: Span::EMPTY,
    });

    let feature_start = doc.features.len() as u32;
    let mut features = Vec::new();
    for child in &group.features {
        for mut feature in
            extract_set_feature(context, layer_ref, net, polarity, source, child, doc)?
        {
            if feature.placement_group.is_some() {
                bail!("nested IPC feature placement groups are not supported");
            }
            feature.placement_group = Some(group_id);
            feature.bbox = doc.placed_paths_bbox(&feature);
            features.push(feature);
        }
    }
    doc.feature_placement_groups[group_id as usize].features =
        Span::new(feature_start, features.len() as u32);
    Ok(features)
}

/// Physical layer order for slot spans, resolved only when a through slot on
/// a spanned fabrication layer will ask for it. An absent stackup leaves
/// declaration order; an invalid one leaves no order at all, so a spanned slot
/// reaches only the layers its span names.
pub(super) fn resolve_slot_layer_order(
    doc: &mut GeometryDocument,
    cad: &ipc2581::types::ecad::CadData,
) -> Option<Vec<Symbol>> {
    let spanned = |layer_ref: Symbol| {
        cad.layers.iter().any(|layer| {
            layer.name == layer_ref && layer.layer_function.is_fabrication() && layer.span.is_some()
        })
    };
    let needed = cad
        .steps
        .iter()
        .flat_map(|step| &step.layer_features)
        .filter(|layer_feature| spanned(layer_feature.layer_ref))
        .flat_map(|layer_feature| &layer_feature.features)
        .any(|feature| matches!(feature, SetFeature::Slot(slot) if !slot.z_axis_dim));
    if !needed {
        return None;
    }
    match physical_stackup_layers(&cad.stackups, &cad.layers) {
        Ok(order) => {
            Some(order.unwrap_or_else(|| cad.layers.iter().map(|layer| layer.name).collect()))
        }
        Err(error) => {
            doc.warn(format!(
                "Spanned slots reach only the layers they name because the stackup is invalid: {error}"
            ));
            None
        }
    }
}

pub(super) fn slot_applies_to_layer(
    source_layer: &Layer,
    target_layer: &Layer,
    layer_order: Option<&[Symbol]>,
    slot: &ipc2581::types::Slot,
) -> bool {
    if source_layer.name == target_layer.name {
        return true;
    }
    if target_layer.layer_function.is_fabrication() || slot.z_axis_dim {
        return false;
    }

    let Some(span) = source_layer.span else {
        return false;
    };
    feature_definitely_spans_layer(
        FeatureSpan::FromTo {
            from: span.from_layer.or_else(|| layer_order?.first().copied()),
            to: span.to_layer.or_else(|| layer_order?.last().copied()),
        },
        target_layer.name,
        layer_order,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn extract_pad(
    context: &ExtractContext<'_>,
    layer_ref: Symbol,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    pad: &ipc2581::types::Pad,
    doc: &mut GeometryDocument,
) -> Result<Option<GeometryFeature>> {
    let Some(x) = pad.x else {
        doc.warn("Skipping pad without x coordinate");
        return Ok(None);
    };
    let Some(y) = pad.y else {
        doc.warn("Skipping pad without y coordinate");
        return Ok(None);
    };
    // A pad may carry its own shape, so a padstack definition refines (hole
    // plating, per-layer shapes) rather than gates.
    let padstack = pad
        .padstack_def_ref
        .and_then(|padstack_ref| context.padstacks.get(&padstack_ref).copied());

    let role = match padstack
        .and_then(|padstack| padstack.hole_def.as_ref())
        .map(|hole| hole.plating_status)
    {
        Some(PlatingStatus::Via | PlatingStatus::ViaCapped) => FeatureRole::Via,
        _ => FeatureRole::Pad,
    };

    let Some(shape) = pad
        .feature
        .as_ref()
        .or_else(|| padstack_pad_shape(padstack?, layer_ref))
    else {
        doc.warn(format!(
            "Skipping pad{} because it has no shape for layer '{}'",
            pad.padstack_def_ref
                .map(|padstack_ref| format!(
                    " of padstack '{}'",
                    context.strings.resolve(padstack_ref)
                ))
                .unwrap_or_default(),
            context.strings.resolve(layer_ref)
        ));
        return Ok(None);
    };
    // Pad Location is the final centre of this layer's shape. KiCad (ShapePos)
    // and Allegro both fold the padstack's shape offset into it, so the pad
    // definition's own Xform and Location describe the padstack and are not
    // applied again.
    let placement = ipc_placement(Point::new(x, y), pad.xform);

    let path_start = doc.arena.paths.len() as u32;
    let Some((void, primitive_ref)) =
        lower_feature_shape(context, doc, shape, placement.transform)?
    else {
        return Ok(None);
    };
    if doc.arena.paths.len() as u32 == path_start {
        return Ok(None);
    }

    let mut feature = lowered_feature(FeatureKind::Padstack, polarity, void, net, source);
    take_pushed_paths(doc, &mut feature, path_start);
    feature.intent.role = role;
    apply_ipc_placement(&mut feature, placement);
    feature.padstack_ref = pad.padstack_def_ref;
    feature.primitive_ref = primitive_ref;
    feature.intent.plating = padstack
        .and_then(|padstack| padstack.hole_def.as_ref())
        .map(|hole| plating_kind(hole.plating_status))
        .unwrap_or(PlatingKind::None);
    feature.clears_previous_in_set = void;
    push_pin_ref(doc, &mut feature, pad.pin_ref.as_ref());

    Ok(Some(feature))
}

/// A feature whose geometry is lowered to paths. A VOID shape clears whatever
/// polarity its set paints.
pub(super) fn lowered_feature(
    kind: FeatureKind,
    polarity: GeometryPolarity,
    void: bool,
    net: Option<Symbol>,
    source: SourceRef,
) -> GeometryFeature {
    let mut feature = GeometryFeature::new(
        kind,
        if void {
            GeometryPolarity::Clear
        } else {
            polarity
        },
    );
    feature.net = net;
    feature.source = source;
    feature
}

/// Give `feature` the paths pushed since `path_start`, and their bounds.
pub(super) fn take_pushed_paths(
    doc: &GeometryDocument,
    feature: &mut GeometryFeature,
    path_start: u32,
) {
    feature.paths = Span::new(path_start, doc.arena.paths.len() as u32 - path_start);
    feature.bbox = doc.arena.paths_bbox(feature.paths);
}

pub(super) fn push_pin_ref(
    doc: &mut GeometryDocument,
    feature: &mut GeometryFeature,
    pin_ref: Option<&ipc2581::types::PinRef>,
) {
    if let Some(pin_ref) = pin_ref {
        feature.pin_refs = Span::single(doc.pin_refs.len() as u32);
        doc.pin_refs.push(PinRef {
            component_ref: pin_ref.component_ref,
            pin: pin_ref.pin,
            title: pin_ref.title,
        });
    }
}

/// The shape a padstack contributes on `layer_ref` when the pad has none of
/// its own.
pub(super) fn padstack_pad_shape(
    padstack: &ipc2581::types::PadStackDef,
    layer_ref: Symbol,
) -> Option<&FeatureShape> {
    [PadUse::Regular, PadUse::Thermal]
        .into_iter()
        .find_map(|pad_use| {
            padstack
                .pad_defs
                .iter()
                .find(|pad_def| pad_def.layer_ref == layer_ref && pad_def.pad_use == pad_use)
        })?
        .feature
        .as_ref()
}

/// Set features carry only a location, never an Xform.
pub(super) fn feature_location_transform(x: f64, y: f64) -> Affine2 {
    Affine2::placement(Point::new(x, y), 0.0, Mirror::NONE, 1.0)
}

/// A dictionary primitive placed directly as a set feature.
#[allow(clippy::too_many_arguments)]
pub(super) fn extract_feature_primitive(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    transform: Affine2,
    shape: &FeatureShape,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    let path_start = doc.arena.paths.len() as u32;
    let Some((void, primitive_ref)) = lower_feature_shape(context, doc, shape, transform)? else {
        return Ok(Vec::new());
    };
    let mut feature = lowered_feature(FeatureKind::Primitive, polarity, void, net, source);
    feature.primitive_ref = primitive_ref;
    primitive_features_from_paths(doc, feature, transform, path_start)
}

pub(super) fn extract_inline_user_primitive(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    primitive: &ipc2581::types::ecad::FeatureUserPrimitive,
    doc: &mut GeometryDocument,
) -> Result<Vec<GeometryFeature>> {
    let transform = feature_location_transform(primitive.x, primitive.y);
    let path_start = doc.arena.paths.len() as u32;
    lower_user_primitive(context, doc, &primitive.primitive, transform)?;
    let feature = lowered_feature(FeatureKind::Primitive, polarity, false, net, source);
    primitive_features_from_paths(doc, feature, transform, path_start)
}

/// One feature per homogeneous run of the paths pushed since `path_start`:
/// fills and strokes of one primitive export and render differently.
pub(super) fn primitive_features_from_paths(
    doc: &GeometryDocument,
    mut feature: GeometryFeature,
    transform: Affine2,
    path_start: u32,
) -> Result<Vec<GeometryFeature>> {
    feature.transform = transform;
    feature.paths = Span::new(path_start, doc.arena.paths.len() as u32 - path_start);
    if feature.paths.is_empty() {
        return Ok(Vec::new());
    }
    process::split_primitive_feature_path_runs(doc, feature).map_err(|error| {
        anyhow::anyhow!("failed to split IPC primitive into homogeneous path features: {error}")
    })
}

pub(super) fn extract_fiducial(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    fiducial: &ipc2581::types::ecad::Fiducial,
    doc: &mut GeometryDocument,
) -> Result<Option<GeometryFeature>> {
    let placement = ipc_placement(
        Point::new(fiducial.location.x, fiducial.location.y),
        fiducial.xform,
    );

    let (primitive, primitive_ref) = match &fiducial.shape {
        ipc2581::types::ecad::FiducialShape::Primitive(primitive) => (primitive, None),
        ipc2581::types::ecad::FiducialShape::StandardPrimitiveRef(id) => {
            let Some(primitive) = context.standard_primitives.get(id).copied() else {
                doc.warn(format!(
                    "Skipping fiducial because standard primitive '{}' is missing",
                    context.strings.resolve(*id)
                ));
                return Ok(None);
            };
            (primitive, Some(PrimitiveRef::Standard(*id)))
        }
    };
    let path_start = doc.arena.paths.len() as u32;
    let void = lower_standard_primitive(context, doc, primitive, placement.transform)?;
    if doc.arena.paths.len() as u32 == path_start {
        return Ok(None);
    }

    let mut feature = lowered_feature(FeatureKind::Primitive, polarity, void, net, source);
    feature.intent.role = FeatureRole::Fiducial;
    feature.fiducial_kind = map_fiducial_kind(fiducial.kind);
    take_pushed_paths(doc, &mut feature, path_start);
    feature.shape = match primitive {
        StandardPrimitive::Circle(circle) => Some(SimpleShape::Circle {
            diameter: circle.shape.diameter * placement.xform.scale,
        }),
        _ => None,
    };
    apply_ipc_placement(&mut feature, placement);
    feature.primitive_ref = primitive_ref;
    push_pin_ref(doc, &mut feature, fiducial.pin_ref.as_ref());
    Ok(Some(feature))
}

pub(super) fn map_fiducial_kind(kind: ipc2581::types::ecad::FiducialKind) -> FiducialKind {
    match kind {
        ipc2581::types::ecad::FiducialKind::BadBoardMark => FiducialKind::BadBoard,
        ipc2581::types::ecad::FiducialKind::Global => FiducialKind::Global,
        ipc2581::types::ecad::FiducialKind::GoodPanelMark => FiducialKind::GoodPanel,
        ipc2581::types::ecad::FiducialKind::Local => FiducialKind::Local,
    }
}

/// A stroked line, arc or polyline, which without a line description has no
/// width to draw.
pub(super) fn extract_stroke(
    context: &ExtractContext<'_>,
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    stroke: &ipc2581::types::Stroke,
    doc: &mut GeometryDocument,
) -> Option<GeometryFeature> {
    let (what, cmds) = match &stroke.path {
        StrokePath::Line(line) => (
            "line",
            vec![
                PathCmd::move_to(Point::new(line.start.x, line.start.y)),
                PathCmd::line_to(Point::new(line.end.x, line.end.y)),
            ],
        ),
        StrokePath::Arc(arc) => (
            "arc",
            vec![
                PathCmd::move_to(Point::new(arc.start.x, arc.start.y)),
                arc_step(arc.end, arc.center, arc.clockwise),
            ],
        ),
        StrokePath::Polyline(polyline) => ("polyline", poly_step_commands(polyline)),
    };
    let (reference, inline) = match stroke.line_desc {
        Some(LineDescGroup::Ref(reference)) => (Some(reference), None),
        Some(LineDescGroup::Inline(line_desc)) => (None, Some(line_desc)),
        None => (None, None),
    };
    let line_desc = require_line_desc(context, doc, what, reference, inline)?;
    let path = doc.push_path(
        stroke_paint(line_desc, 1.0),
        [ContourBuf::new(cmds).with_consistent_arcs()],
    );
    let mut feature = GeometryFeature::new(FeatureKind::Trace, polarity);
    feature.net = net;
    feature.source = source;
    feature.bbox = doc.arena.paths[path as usize].bbox;
    feature.paths = Span::single(path);
    Some(feature)
}

pub(super) fn extract_polygon(
    net: Option<Symbol>,
    polarity: GeometryPolarity,
    source: SourceRef,
    polygon: &ipc2581::types::Polygon,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let path_start = doc.arena.paths.len() as u32;
    doc.push_path(
        Paint::Fill {
            rule: FillRule::NonZero,
        },
        [polygon_contour(polygon)],
    );
    let mut feature = lowered_feature(FeatureKind::Polygon, polarity, false, net, source);
    take_pushed_paths(doc, &mut feature, path_start);
    feature
}

pub(super) fn extract_hole(
    source: SourceRef,
    padstack_ref: Option<Symbol>,
    hole: &ipc2581::types::Hole,
    doc: &mut GeometryDocument,
) -> GeometryFeature {
    let placement = ipc_placement(Point::new(hole.x, hole.y), hole.xform);
    let path_start = doc.arena.paths.len() as u32;
    let size = hole.diameter * placement.xform.scale;
    let (outline, shape) = match hole.shape {
        IpcHoleShape::Circle => (
            shapes::ellipse(hole.diameter, hole.diameter),
            SimpleShape::Circle { diameter: size },
        ),
        IpcHoleShape::Square => (
            shapes::rect(hole.diameter, hole.diameter),
            SimpleShape::Square { side: size },
        ),
    };
    push_filled_shape(doc, placement.transform, outline);

    let mut feature = GeometryFeature::new(FeatureKind::Hole, GeometryPolarity::Dark);
    feature.source = source;
    feature.source_name = hole.name;
    feature.spec_refs = push_spec_refs(doc, &hole.spec_refs);
    take_pushed_paths(doc, &mut feature, path_start);
    feature.shape = Some(shape);
    apply_ipc_placement(&mut feature, placement);
    feature.padstack_ref = padstack_ref;
    feature.intent.plating = plating_kind(hole.plating_status);
    feature
}

pub(super) fn extract_slot(
    context: &ExtractContext<'_>,
    source: SourceRef,
    padstack_ref: Option<Symbol>,
    slot: &ipc2581::types::Slot,
    doc: &mut GeometryDocument,
) -> Result<GeometryFeature> {
    let placement = ipc_placement(Point::new(slot.x, slot.y), slot.xform);
    let path_start = doc.arena.paths.len() as u32;
    let mut shape = None;

    match &slot.shape {
        SlotShape::Outline(polygon) => {
            push_polygon_path(doc, polygon, placement.transform, FillRule::NonZero);
        }
        SlotShape::Primitive(primitive) => {
            if let StandardPrimitive::Oval(oval) = primitive {
                shape = Some(SimpleShape::Oval {
                    width: oval.shape.size.width * placement.xform.scale,
                    height: oval.shape.size.height * placement.xform.scale,
                });
            }
            let _ = lower_standard_primitive(context, doc, primitive, placement.transform)?;
        }
    }

    let mut feature = GeometryFeature::new(FeatureKind::Slot, GeometryPolarity::Dark);
    feature.source = source;
    feature.source_name = slot.name;
    take_pushed_paths(doc, &mut feature, path_start);
    feature.shape = shape;
    apply_ipc_placement(&mut feature, placement);
    feature.padstack_ref = padstack_ref;
    feature.intent.plating = plating_kind(slot.plating_status);
    Ok(feature)
}
