use anyhow::Result;
use ipc2581::Symbol;
use ipc2581::types;

use super::ImportedDesign;
use crate::dialects::assembly as ir;
use crate::dialects::ipc::ArtworkScope;
use crate::geom::Point;

impl ImportedDesign {
    /// Lower source-faithful IPC-2581 assembly data into the canonical
    /// assembly dialect for one explicit layout scope.
    pub fn assembly_document(&self, scope: ir::Scope) -> Result<ir::Document> {
        let artwork_scope = match scope {
            ir::Scope::Board => ArtworkScope::Board,
            ir::Scope::BoardArray => ArtworkScope::ArrayFlattened,
        };
        let primary_bom = self
            .content
            .bom_refs
            .iter()
            .find_map(|reference| self.boms.iter().position(|bom| bom.name == *reference))
            .map(|index| index as u32)
            .or_else(|| (!self.boms.is_empty()).then_some(0));
        let occurrences = self
            .component_occurrences(artwork_scope)?
            .into_iter()
            .map(|occurrence| ir::ComponentOccurrence {
                id: occurrence.id,
                root_from_component: occurrence.root_from_component,
                board: occurrence.board,
                root_from_board: occurrence.root_from_board,
                board_from_component: occurrence.board_from_component,
                population: occurrence.population,
            })
            .collect();

        Ok(ir::Document {
            scope,
            root_step: self
                .step_occurrences(artwork_scope)?
                .first()
                .map(|occurrence| occurrence.step),
            steps: self
                .geometry
                .layout
                .steps
                .iter()
                .map(|step| ir::Step {
                    name: self.resolve(step.source_step_ref).to_owned(),
                    kind: step.kind,
                })
                .collect(),
            primary_bom,
            boms: self.boms.iter().map(|bom| map_bom(self, bom)).collect(),
            avl: self.avl.as_ref().map(|avl| map_avl(self, avl)),
            packages: self
                .packages
                .iter()
                .enumerate()
                .map(|(index, package)| map_package(self, index as u32, package))
                .collect(),
            components: self
                .components
                .iter()
                .enumerate()
                .map(|(index, component)| map_component(self, index as u32, component))
                .collect(),
            occurrences,
        })
    }
}

fn map_component(
    design: &ImportedDesign,
    index: u32,
    component: &super::ComponentDefinition,
) -> ir::ComponentDefinition {
    let source = &component.source;
    let side = design
        .layer_definitions
        .iter()
        .find(|layer| layer.name == source.layer_ref)
        .and_then(|layer| layer.side)
        .map(map_side)
        .unwrap_or(ir::Side::Unspecified);
    ir::ComponentDefinition {
        id: ir::ComponentDefinitionId(index),
        step: component.step,
        source_index: component.source_index,
        designator: resolve_optional(design, source.ref_des),
        package_ref: resolve_optional(design, source.package_ref),
        package: component.package,
        material_designator: resolve_optional(design, source.mat_des),
        layer_ref: design.resolve(source.layer_ref).to_owned(),
        topside_layer_ref: resolve_optional(design, source.layer_ref_topside),
        side,
        mount: map_mount(source.mount_type),
        part: design.resolve(source.part).to_owned(),
        model_ref: resolve_optional(design, source.model_ref),
        weight: source.weight,
        height: source.height,
        standoff: source.standoff,
        source_transform: source.xform.map(map_transform),
        local_from_component: component.local_from_component,
        nonstandard_attributes: source
            .nonstandard_attributes
            .iter()
            .map(|attribute| ir::Attribute {
                name: design.resolve(attribute.name).to_owned(),
                value: resolve_optional(design, attribute.value),
                attribute_type: resolve_optional(design, attribute.attr_type),
            })
            .collect(),
        slot_cavity_ref: resolve_optional(design, source.slot_cavity_ref),
        spec_refs: resolve_all(design, &source.spec_refs),
        bom_references: component.bom_references.clone(),
        population: component.population,
    }
}

fn map_package(
    design: &ImportedDesign,
    index: u32,
    package: &super::PackageDefinition,
) -> ir::PackageDefinition {
    let source = &package.source;
    let mut pins = source
        .pins
        .iter()
        .map(|pin| map_package_pin(design, ir::PackagePinView::Primary, pin))
        .collect::<Vec<_>>();
    if let Some(topside) = &source.topside {
        pins.extend(
            topside
                .pins
                .iter()
                .map(|pin| map_package_pin(design, ir::PackagePinView::Topside, pin)),
        );
    }
    ir::PackageDefinition {
        id: ir::PackageDefinitionId(index),
        step: package.step,
        source_index: package.source_index,
        name: design.resolve(source.name).to_owned(),
        package_type: design.resolve(source.package_type).to_owned(),
        pin_one: resolve_optional(design, source.pin_one),
        pin_one_orientation: resolve_optional(design, source.pin_one_orientation),
        height: source.height,
        negative_body_extension: source.negative_body_extension,
        comment: resolve_optional(design, source.comment),
        pickup_point: source
            .pickup_point
            .map(|location| Point::new(location.x, location.y)),
        pins,
    }
}

fn map_package_pin(
    design: &ImportedDesign,
    view: ir::PackagePinView,
    pin: &types::PackagePin,
) -> ir::PackagePin {
    ir::PackagePin {
        view,
        number: design.resolve(pin.number).to_owned(),
        name: resolve_optional(design, pin.name),
        pin_type: match pin.pin_type {
            types::PackagePinType::Through => ir::PackagePinType::Through,
            types::PackagePinType::Blind => ir::PackagePinType::Blind,
            types::PackagePinType::Surface => ir::PackagePinType::Surface,
        },
        electrical_type: pin
            .electrical_type
            .map(|electrical_type| match electrical_type {
                types::PackagePinElectricalType::Electrical => {
                    ir::PackagePinElectricalType::Electrical
                }
                types::PackagePinElectricalType::Mechanical => {
                    ir::PackagePinElectricalType::Mechanical
                }
                types::PackagePinElectricalType::Undefined => {
                    ir::PackagePinElectricalType::Undefined
                }
            }),
        mount_type: pin.mount_type.map(|mount| match mount {
            types::PackagePinMountType::SurfaceMountPin => ir::PackagePinMountType::SurfaceMountPin,
            types::PackagePinMountType::SurfaceMountPad => ir::PackagePinMountType::SurfaceMountPad,
            types::PackagePinMountType::ThroughHolePin => ir::PackagePinMountType::ThroughHolePin,
            types::PackagePinMountType::ThroughHoleHole => ir::PackagePinMountType::ThroughHoleHole,
            types::PackagePinMountType::PressFit => ir::PackagePinMountType::PressFit,
            types::PackagePinMountType::NonBoard => ir::PackagePinMountType::NonBoard,
            types::PackagePinMountType::Hole => ir::PackagePinMountType::Hole,
            types::PackagePinMountType::WireBond => ir::PackagePinMountType::WireBond,
            types::PackagePinMountType::Undefined => ir::PackagePinMountType::Undefined,
        }),
        polarity: pin.polarity.map(|polarity| match polarity {
            types::PackagePinPolarity::Plus => ir::PackagePinPolarity::Plus,
            types::PackagePinPolarity::Minus => ir::PackagePinPolarity::Minus,
            types::PackagePinPolarity::Anode => ir::PackagePinPolarity::Anode,
            types::PackagePinPolarity::Cathode => ir::PackagePinPolarity::Cathode,
        }),
        location: pin
            .location
            .map(|location| Point::new(location.x, location.y)),
        transform: pin.xform.map(map_transform),
    }
}

fn map_bom(design: &ImportedDesign, bom: &types::Bom) -> ir::Bom {
    ir::Bom {
        name: design.resolve(bom.name).to_owned(),
        header: bom.header.as_ref().map(|header| ir::BomHeader {
            assembly: design.resolve(header.assembly).to_owned(),
            revision: design.resolve(header.revision).to_owned(),
            affecting: header.affecting,
            step_refs: resolve_all(design, &header.step_refs),
        }),
        items: bom
            .items
            .iter()
            .map(|item| ir::BomItem {
                oem_design_number: design.resolve(item.oem_design_number_ref).to_owned(),
                quantity: item.quantity,
                quantity_raw: design.resolve(item.quantity_raw).to_owned(),
                pin_count: item.pin_count,
                pin_count_raw: resolve_optional(design, item.pin_count_raw),
                category: item.category.map(map_bom_category),
                internal_part_number: resolve_optional(design, item.internal_part_number),
                description: resolve_optional(design, item.description),
                designators: item
                    .designators
                    .iter()
                    .map(|designator| map_bom_designator(design, designator))
                    .collect(),
                characteristics: item
                    .characteristics
                    .as_ref()
                    .map(|characteristics| map_characteristics(design, characteristics)),
                spec_refs: resolve_all(design, &item.spec_refs),
            })
            .collect(),
    }
}

fn map_bom_designator(
    design: &ImportedDesign,
    designator: &types::BomDesignator,
) -> ir::BomDesignator {
    match designator {
        types::BomDesignator::Reference(reference) => {
            ir::BomDesignator::Reference(ir::ReferenceDesignator {
                name: design.resolve(reference.name).to_owned(),
                package_ref: resolve_optional(design, reference.package_ref),
                populate: reference.populate,
                layer_ref: resolve_optional(design, reference.layer_ref),
                model_ref: resolve_optional(design, reference.model_ref),
                tunings: reference
                    .tunings
                    .iter()
                    .map(|tuning| ir::Tuning {
                        value: design.resolve(tuning.value).to_owned(),
                        comments: resolve_optional(design, tuning.comments),
                    })
                    .collect(),
                firmwares: reference
                    .firmwares
                    .iter()
                    .map(|firmware| ir::Firmware {
                        program_name: design.resolve(firmware.program_name).to_owned(),
                        program_version: design.resolve(firmware.program_version).to_owned(),
                        file_name: design.resolve(firmware.file.name).to_owned(),
                        file_crc: design.resolve(firmware.file.crc).to_owned(),
                        payload: match firmware.payload {
                            types::BomFirmwarePayload::Reference(value) => {
                                ir::FirmwarePayload::Reference(design.resolve(value).to_owned())
                            }
                            types::BomFirmwarePayload::Cached(value) => {
                                ir::FirmwarePayload::Cached(design.resolve(value).to_owned())
                            }
                        },
                    })
                    .collect(),
            })
        }
        types::BomDesignator::Material(named) => {
            ir::BomDesignator::Material(map_named_designator(design, named))
        }
        types::BomDesignator::Document(named) => {
            ir::BomDesignator::Document(map_named_designator(design, named))
        }
        types::BomDesignator::Tool(named) => {
            ir::BomDesignator::Tool(map_named_designator(design, named))
        }
        types::BomDesignator::Find(find) => ir::BomDesignator::Find(ir::FindDesignator {
            number: find.number,
            number_raw: design.resolve(find.number_raw).to_owned(),
            layer_ref: resolve_optional(design, find.layer_ref),
            model_ref: resolve_optional(design, find.model_ref),
        }),
    }
}

fn map_named_designator(
    design: &ImportedDesign,
    named: &types::BomNamedDesignator,
) -> ir::NamedDesignator {
    ir::NamedDesignator {
        name: design.resolve(named.name).to_owned(),
        layer_ref: resolve_optional(design, named.layer_ref),
    }
}

fn map_characteristics(
    design: &ImportedDesign,
    characteristics: &types::Characteristics,
) -> ir::Characteristics {
    let mut values = Vec::new();
    values.extend(
        characteristics
            .measured
            .iter()
            .map(|value| ir::Characteristic::Measured {
                definition_source: resolve_optional(design, value.definition_source),
                name: resolve_optional(design, value.name),
                value: value.value,
                engineering_unit: resolve_optional(design, value.engineering_unit),
                negative_tolerance: value.negative_tolerance,
                positive_tolerance: value.positive_tolerance,
            }),
    );
    values.extend(
        characteristics
            .ranged
            .iter()
            .map(|value| ir::Characteristic::Ranged {
                definition_source: resolve_optional(design, value.definition_source),
                name: resolve_optional(design, value.name),
                lower_value: value.lower_value,
                upper_value: value.upper_value,
                engineering_unit: resolve_optional(design, value.engineering_unit),
                negative_tolerance: value.negative_tolerance,
                positive_tolerance: value.positive_tolerance,
            }),
    );
    values.extend(
        characteristics
            .enumerated
            .iter()
            .map(|value| ir::Characteristic::Enumerated {
                definition_source: resolve_optional(design, value.definition_source),
                name: resolve_optional(design, value.name),
                value: resolve_optional(design, value.value),
            }),
    );
    values.extend(
        characteristics
            .textuals
            .iter()
            .map(|value| ir::Characteristic::Textual {
                definition_source: resolve_optional(design, value.definition_source),
                name: resolve_optional(design, value.name),
                value: resolve_optional(design, value.value),
            }),
    );
    ir::Characteristics {
        category: characteristics.category.map(map_bom_category),
        values,
    }
}

fn map_avl(design: &ImportedDesign, avl: &types::Avl) -> ir::Avl {
    ir::Avl {
        name: design.resolve(avl.name).to_owned(),
        header: avl.header.as_ref().map(|header| ir::AvlHeader {
            title: design.resolve(header.title).to_owned(),
            source: design.resolve(header.source).to_owned(),
            author: design.resolve(header.author).to_owned(),
            datetime: design.resolve(header.datetime).to_owned(),
            version: header.version,
            comment: resolve_optional(design, header.comment),
            modification_ref: resolve_optional(design, header.mod_ref),
        }),
        items: avl
            .items
            .iter()
            .map(|item| ir::AvlItem {
                oem_design_number: design.resolve(item.oem_design_number).to_owned(),
                alternatives: item
                    .vmpn_list
                    .iter()
                    .map(|part| ir::ApprovedPart {
                        external_vendor: resolve_optional(design, part.evpl_vendor),
                        external_mpn: resolve_optional(design, part.evpl_mpn),
                        qualified: part.qualified,
                        chosen: part.chosen,
                        manufacturer_parts: part
                            .mpns
                            .iter()
                            .map(|mpn| ir::ManufacturerPart {
                                name: design.resolve(mpn.name).to_owned(),
                                rank: mpn.rank,
                                cost: mpn.cost,
                                moisture_sensitivity: mpn
                                    .moisture_sensitivity
                                    .map(map_moisture_sensitivity),
                                available: mpn.availability,
                                other: resolve_optional(design, mpn.other),
                            })
                            .collect(),
                        vendor_refs: part
                            .vendors
                            .iter()
                            .map(|vendor| design.resolve(vendor.enterprise_ref).to_owned())
                            .collect(),
                    })
                    .collect(),
                spec_refs: resolve_all(design, &item.spec_refs),
            })
            .collect(),
    }
}

fn map_side(side: types::Side) -> ir::Side {
    match side {
        types::Side::Top => ir::Side::Top,
        types::Side::Bottom => ir::Side::Bottom,
        types::Side::Both => ir::Side::Both,
        types::Side::Internal => ir::Side::Internal,
        types::Side::All => ir::Side::All,
        types::Side::None => ir::Side::None,
    }
}

fn map_mount(mount: types::MountType) -> ir::Mount {
    match mount {
        types::MountType::Smt => ir::Mount::Smt,
        types::MountType::Thmt => ir::Mount::ThroughHole,
        types::MountType::Embedded => ir::Mount::Embedded,
        types::MountType::PressFit => ir::Mount::PressFit,
        types::MountType::WireBonded => ir::Mount::WireBonded,
        types::MountType::Glued => ir::Mount::Glued,
        types::MountType::Clamped => ir::Mount::Clamped,
        types::MountType::Socketed => ir::Mount::Socketed,
        types::MountType::Formed => ir::Mount::Formed,
        types::MountType::Other => ir::Mount::Other,
    }
}

fn map_transform(transform: types::Xform) -> ir::Transform {
    ir::Transform {
        x_offset: transform.x_offset,
        y_offset: transform.y_offset,
        rotation_degrees: transform.rotation,
        mirror: transform.mirror,
        face_up: transform.face_up,
        scale: transform.scale,
    }
}

fn map_bom_category(category: types::BomCategory) -> ir::BomCategory {
    match category {
        types::BomCategory::Electrical => ir::BomCategory::Electrical,
        types::BomCategory::Programmable => ir::BomCategory::Programmable,
        types::BomCategory::Mechanical => ir::BomCategory::Mechanical,
        types::BomCategory::Material => ir::BomCategory::Material,
        types::BomCategory::Document => ir::BomCategory::Document,
    }
}

fn map_moisture_sensitivity(sensitivity: types::MoistureSensitivity) -> ir::MoistureSensitivity {
    match sensitivity {
        types::MoistureSensitivity::Unlimited => ir::MoistureSensitivity::Unlimited,
        types::MoistureSensitivity::OneYear => ir::MoistureSensitivity::OneYear,
        types::MoistureSensitivity::FourWeeks => ir::MoistureSensitivity::FourWeeks,
        types::MoistureSensitivity::Hours168 => ir::MoistureSensitivity::Hours168,
        types::MoistureSensitivity::Hours72 => ir::MoistureSensitivity::Hours72,
        types::MoistureSensitivity::Hours48 => ir::MoistureSensitivity::Hours48,
        types::MoistureSensitivity::Hours24 => ir::MoistureSensitivity::Hours24,
        types::MoistureSensitivity::Bake => ir::MoistureSensitivity::Bake,
    }
}

fn resolve_optional(design: &ImportedDesign, symbol: Option<Symbol>) -> Option<String> {
    symbol.map(|symbol| design.resolve(symbol).to_owned())
}

fn resolve_all(design: &ImportedDesign, symbols: &[Symbol]) -> Vec<String> {
    symbols
        .iter()
        .map(|symbol| design.resolve(*symbol).to_owned())
        .collect()
}
