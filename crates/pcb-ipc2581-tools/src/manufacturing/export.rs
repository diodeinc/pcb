use pcb_ir::geom::Resolution;
#[cfg(feature = "cli")]
use std::fs;
#[cfg(feature = "cli")]
use std::io::BufWriter;
use std::io::{Cursor, Seek, Write};
#[cfg(feature = "cli")]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pcb_ir::dialects::ipc::{ArtworkScope, LayoutStepKind};
use pcb_ir::import::ipc2581::ImportedDesign;
#[cfg(feature = "cli")]
use pcb_ir::import::ipc2581::import_design;
use zip::{ZipWriter, write::FileOptions};

use crate::gerber;
#[cfg(feature = "cli")]
use crate::ipc2581 as ipc;

#[derive(Debug, Clone)]
pub struct ManufacturingExportOptions {
    pub view: ArtworkScope,
    /// Include assembly, fabrication drawing, glue, courtyard, and document layers.
    pub include_auxiliary_layers: bool,
    /// Write the V-score relief construction as debug SVGs into this
    /// directory.
    pub relief_debug_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ManufacturingPackage {
    pub files: Vec<ManufacturingFile>,
}

impl ManufacturingPackage {
    /// Serialize the complete manufacturing package to an in-memory ZIP archive.
    pub fn to_zip(&self) -> Result<Vec<u8>> {
        Ok(write_zip(self, Cursor::new(Vec::new()))?.into_inner())
    }
}

#[derive(Debug, Clone)]
pub struct ManufacturingFile {
    pub filename: String,
    pub contents: String,
}

/// Every Gerber X2 layer, XNC drill files, and Gerber job for one artwork scope.
pub fn build_manufacturing_package(
    imported: &ImportedDesign,
    options: &ManufacturingExportOptions,
    resolution: Resolution,
) -> Result<ManufacturingPackage> {
    let mut files = gerber::build_gerber_x2_files(
        imported,
        options.view,
        &gerber::GerberExportOptions {
            relief_debug_dir: options.relief_debug_dir.clone(),
            include_auxiliary_layers: options.include_auxiliary_layers,
        },
        resolution,
    )?
    .into_iter()
    .map(|file| ManufacturingFile {
        filename: file.filename,
        contents: file.contents,
    })
    .collect::<Vec<_>>();
    files.extend(super::drill::build_xnc_drill_files_from_design(
        imported,
        options.view,
    )?);
    files.push(build_gerber_job(imported, options.view, &files)?);

    Ok(ManufacturingPackage { files })
}

/// Ucamco Gerber Job revision 2020.08, sections 2.4–2.7:
/// https://www.ucamco.com/files/downloads/file_en/435/gerber-job-format-specification-revision-2020-08_en.pdf
/// Missing characteristics are undefined, not defaults. In particular, IPC's
/// material specs do not supply CAD clearance/width design rules.
fn build_gerber_job(
    imported: &ImportedDesign,
    scope: ArtworkScope,
    files: &[ManufacturingFile],
) -> Result<ManufacturingFile> {
    use ipc2581::types::{FinishType, LayerFunction, WhereMeasured};
    use serde_json::json;

    // Read the attributes we actually emitted, not a second filename/layer map.
    // This also handles opt-in layers and XNC files without phantom entries.
    let inventory = files
        .iter()
        .map(|file| {
            let xnc = file.contents.starts_with("M48");
            let mut entry = json!({
                "Path": file.filename,
                "FileFormat": if xnc { "XNC" } else { "Gerber" },
            });
            for line in file.contents.lines() {
                let attribute = if xnc {
                    line.strip_prefix("; #@! TF.")
                } else {
                    line.strip_prefix("%TF.").and_then(|s| s.strip_suffix("*%"))
                };
                if let Some((name, value)) = attribute.and_then(|s| s.split_once(','))
                    && matches!(name, "FileFunction" | "FilePolarity")
                {
                    entry[name] = json!(value);
                }
            }
            entry
        })
        .collect::<Vec<_>>();
    let mut general = json!({});
    // The job spec describes a single PCB, not its assembly panel. Keep the
    // single-board dimensions for repeated arrays, but omit them for panels
    // containing distinct board steps. Unused boards are not a fallback.
    let board_bounds = imported
        .layout_occurrences(if scope == ArtworkScope::Board {
            ArtworkScope::Board
        } else {
            ArtworkScope::ArrayFlattened
        })
        .ok()
        .and_then(|occurrences| {
            let boards = occurrences
                .iter()
                .map(|(step, _)| *step)
                .filter(|step| {
                    imported.geometry.layout.steps[*step as usize].kind == LayoutStepKind::Board
                })
                .collect::<std::collections::HashSet<_>>();
            (boards.len() == 1).then(|| {
                imported.geometry.layout.steps[*boards.iter().next().unwrap() as usize].bbox
            })
        });
    if let Some(bounds) = board_bounds.filter(|bounds| !bounds.is_empty()) {
        general["Size"] = json!({ "X": bounds.width(), "Y": bounds.height() });
    }
    let copper_count = imported
        .layer_definitions
        .iter()
        .filter(|layer| crate::layers::is_copper(layer.layer_function))
        .count();
    if copper_count > 0 {
        general["LayerNumber"] = json!(copper_count);
    }
    let mut job = json!({
        "Header": {
            "GenerationSoftware": {
                "Vendor": "Diode", "Application": "pcb", "Version": env!("CARGO_PKG_VERSION")
            },
            "Comment": "Size describes the single board; file attributes describe the exported artwork scope."
        },
        "FilesAttributes": inventory,
    });

    // Multiple stackups need region/substack associations that this importer
    // does not expose. Do not arbitrarily choose one as the whole board.
    if let [stackup] = imported.stackups.as_slice() {
        // Gerber BoardThickness includes copper but excludes mask. IPC can
        // explicitly measure at mask or laminate instead; those are not equal.
        if stackup.where_measured == Some(WhereMeasured::Metal)
            && let Some(thickness) = stackup.overall_thickness
        {
            general["BoardThickness"] = json!(thickness);
        }
        let mut layers = stackup.layers.iter().collect::<Vec<_>>();
        let numbers = layers
            .iter()
            .filter_map(|layer| layer.layer_number)
            .collect::<std::collections::HashSet<_>>();
        let ordered = numbers.len() == layers.len()
            || layers.iter().all(|layer| layer.layer_number.is_none());
        if numbers.len() == layers.len() {
            layers.sort_by_key(|layer| layer.layer_number);
        }
        let unique = layers
            .iter()
            .map(|layer| layer.layer_ref)
            .collect::<std::collections::HashSet<_>>()
            .len()
            == layers.len();
        // Require coverage of known planar materials, not just resolution of
        // the rows that happen to be present. Glue and hole-protection coating
        // artwork are not necessarily layers in the bare-board stack.
        let complete = imported.layer_definitions.iter().all(|definition| {
            let function = definition.layer_function;
            let planar = crate::layers::is_copper(function)
                || function.is_dielectric()
                || matches!(
                    function,
                    LayerFunction::Soldermask
                        | LayerFunction::Silkscreen
                        | LayerFunction::Legend
                        | LayerFunction::Solderpaste
                        | LayerFunction::Pastemask
                );
            !planar
                || layers
                    .iter()
                    .any(|layer| layer.layer_ref == definition.name)
        });
        let mut finishes = std::collections::BTreeSet::new();
        let mut unknown_finish = false;
        let materials = layers
            .iter()
            .map(|layer| {
                let definition = imported
                    .layer_definitions
                    .iter()
                    .find(|definition| definition.name == layer.layer_ref)?;
                let function = definition.layer_function;
                let kind = match function {
                    f if crate::layers::is_copper(f) => "Copper",
                    LayerFunction::DielBase
                    | LayerFunction::DielCore
                    | LayerFunction::DielPreg
                    | LayerFunction::DielAdhv
                    | LayerFunction::DielBondPly => "Dielectric",
                    LayerFunction::DielCoverlay => "CoverLay",
                    LayerFunction::Soldermask => "SolderMask",
                    LayerFunction::Silkscreen | LayerFunction::Legend => "Legend",
                    f => f.as_str(),
                };
                let spec = layer.spec_ref.and_then(|name| imported.specs.get(&name));
                if matches!(
                    function,
                    LayerFunction::CoatingCond | LayerFunction::CoatingNonCond
                ) {
                    if let Some(finish) = spec.and_then(|spec| spec.surface_finish.as_ref()) {
                        // Preserve ambiguous IPC tokens rather than asserting, e.g.,
                        // that solder type S is specifically lead-free HASL.
                        finishes.insert(match finish.finish_type {
                            FinishType::EnigN | FinishType::EnigG => "ENIG",
                            FinishType::EnepigN | FinishType::EnepigG | FinishType::EnepigP => {
                                "ENEPIG"
                            }
                            FinishType::IAg => "Immersion silver",
                            FinishType::ISn => "Immersion tin",
                            f => f.as_str(),
                        });
                    } else {
                        unknown_finish = true;
                    }
                }
                let mut material = json!({"Type": kind, "Name": imported.resolve(layer.layer_ref)});
                if let Some(name) = layer.material.or_else(|| spec.and_then(|s| s.material)) {
                    material["Material"] = json!(imported.resolve(name));
                }
                if let Some((r, g, b)) = spec.and_then(|s| s.color_rgb) {
                    material["Color"] = json!(format!("R{r:03}G{g:03}B{b:03}"));
                } else if let Some(color) = spec.and_then(|s| {
                    s.color_term
                        .and_then(|color| gerber_job_color(imported.resolve(color)))
                        .or_else(|| {
                            // KiCad writes material colors as free-text IPC
                            // properties, rather than Color/ColorTerm elements.
                            s.properties.iter().find_map(|property| {
                                imported
                                    .resolve(*property)
                                    .strip_prefix("Color : ")
                                    .and_then(gerber_job_color)
                            })
                        })
                }) {
                    material["Color"] = json!(color);
                }
                for (key, value) in [
                    ("Thickness", layer.thickness),
                    (
                        "DielectricConstant",
                        layer
                            .dielectric_constant
                            .or_else(|| spec.and_then(|s| s.dielectric_constant)),
                    ),
                    (
                        "LossTangent",
                        layer
                            .loss_tangent
                            .or_else(|| spec.and_then(|s| s.loss_tangent)),
                    ),
                ] {
                    if let Some(value) = value {
                        material[key] = json!(value);
                    }
                }
                Some(material)
            })
            .collect::<Option<Vec<_>>>();
        // A partial material stack is forbidden by section 2.5.
        if let Some(materials) =
            materials.filter(|layers| complete && ordered && unique && !layers.is_empty())
        {
            job["MaterialStackup"] = json!(materials);
            if !unknown_finish && finishes.len() == 1 && !finishes.contains("OTHER") {
                general["Finish"] = json!(finishes.first().unwrap());
            }
        }
    }
    job["GeneralSpecs"] = general;
    Ok(ManufacturingFile {
        filename: "job.gbrjob".to_owned(),
        contents: serde_json::to_string_pretty(&job)? + "\n",
    })
}

fn gerber_job_color(value: &str) -> Option<String> {
    if let Some(name) = ["Red", "Yellow", "Green", "Blue", "White", "Black"]
        .into_iter()
        .find(|name| name.eq_ignore_ascii_case(value))
    {
        return Some(name.to_owned());
    }
    let hex = value.strip_prefix('#')?;
    if !matches!(hex.len(), 6 | 8) || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    // KiCad's optional alpha is display transparency, not a fabrication color.
    // Gerber job RGB has no alpha component.
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(format!("R{r:03}G{g:03}B{b:03}"))
}

/// Parse, import, build, and write a manufacturing package to `output`: a
/// zip when the path has a `.zip` extension, otherwise a directory.
#[cfg(feature = "cli")]
pub fn export_manufacturing_package(
    input_file: &Path,
    output: &Path,
    options: &ManufacturingExportOptions,
    resolution: Resolution,
) -> Result<ManufacturingPackage> {
    let content = crate::utils::file::load_ipc_file(input_file)?;
    let ipc = ipc::Ipc2581::parse(&content)?;
    let package =
        build_manufacturing_package(&import_design(&ipc, resolution)?, options, resolution)?;
    write_manufacturing_package(&package, output)?;
    Ok(package)
}

#[cfg(feature = "cli")]
pub fn write_manufacturing_package(package: &ManufacturingPackage, output: &Path) -> Result<()> {
    if output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        write_manufacturing_zip(package, output)
    } else {
        write_manufacturing_directory(package, output)
    }
}

#[cfg(feature = "cli")]
fn write_manufacturing_directory(package: &ManufacturingPackage, output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create manufacturing output directory {}",
            output_dir.display()
        )
    })?;
    // An opt-out export must not succeed with older auxiliary Gerbers still
    // present. Refuse before writing anything rather than delete user files.
    for entry in fs::read_dir(output_dir)? {
        let path = entry?.path();
        let auxiliary_extension =
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ["gbr", "gta", "gba"]
                        .iter()
                        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
                });
        if auxiliary_extension {
            anyhow::ensure!(
                package
                    .files
                    .iter()
                    .any(|file| path.file_name() == Some(file.filename.as_ref())),
                "output directory contains a Gerber not in this package: {}; use a fresh directory or a ZIP output",
                path.display()
            );
        }
    }
    for file in &package.files {
        fs::write(output_dir.join(&file.filename), &file.contents).with_context(|| {
            format!(
                "failed to write manufacturing file {}",
                output_dir.join(&file.filename).display()
            )
        })?;
    }
    Ok(())
}

#[cfg(feature = "cli")]
fn write_manufacturing_zip(package: &ManufacturingPackage, output_zip: &Path) -> Result<()> {
    if let Some(parent) = output_zip.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create manufacturing zip output directory {}",
                parent.display()
            )
        })?;
    }

    let zip_file = fs::File::create(output_zip).with_context(|| {
        format!(
            "failed to create manufacturing zip {}",
            output_zip.display()
        )
    })?;
    write_zip(package, BufWriter::new(zip_file))?;
    Ok(())
}

fn write_zip<W: Write + Seek>(package: &ManufacturingPackage, writer: W) -> Result<W> {
    let mut zip = ZipWriter::new(writer);
    for file in &package.files {
        zip.start_file(&file.filename, FileOptions::<()>::default())
            .with_context(|| format!("failed to add {} to manufacturing zip", file.filename))?;
        zip.write_all(file.contents.as_bytes())
            .with_context(|| format!("failed to write {} to manufacturing zip", file.filename))?;
    }
    zip.finish().context("failed to finalize manufacturing zip")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::import::ipc2581::import_design;
    use serde_json::{Value, json};
    use std::io::Read;

    // Inch input, asymmetric offset board, larger repeated panel, and stackup
    // declaration order different from sequence order catch unit/scope mistakes.
    const JOB_BOARD: &str = r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
      <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="panel"/></Content>
      <Ecad><CadHeader units="INCH">
        <Spec name="core"><General type="MATERIAL"><Property text="FR4"/></General>
          <Dielectric type="DIELECTRIC_CONSTANT"><Property value="4.2"/></Dielectric>
          <Dielectric type="LOSS_TANGENT"><Property value="0.018"/></Dielectric></Spec>
        <Spec name="finish"><SurfaceFinish type="ENIG-N"/></Spec>
        <Spec name="unused"><SurfaceFinish type="OSP"/></Spec>
        <Spec name="ink"><General type="MATERIAL"><ColorTerm name="GREEN"/></General></Spec>
      </CadHeader><CadData>
        <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
        <Layer name="BOTTOM" layerFunction="SIGNAL" side="BOTTOM" polarity="POSITIVE"/>
        <Layer name="CORE" layerFunction="DIELCORE" side="INTERNAL"/>
        <Layer name="MASK" layerFunction="SOLDERMASK" side="TOP" polarity="POSITIVE"/>
        <Layer name="FINISH" layerFunction="COATINGCOND" side="TOP"/>
        <Layer name="DRILL" layerFunction="DRILL" side="ALL" polarity="POSITIVE">
          <Span fromLayer="TOP" toLayer="BOTTOM"/></Layer>
        <Stackup name="stack" overallThickness="0.062" whereMeasured="METAL">
          <StackupGroup name="all">
            <StackupLayer layerOrGroupRef="BOTTOM" thickness="0.002" sequence="5"/>
            <StackupLayer layerOrGroupRef="MASK" sequence="1"><SpecRef id="ink"/></StackupLayer>
            <StackupLayer layerOrGroupRef="TOP" thickness="0.001" sequence="3"/>
            <StackupLayer layerOrGroupRef="CORE" thickness="0.059" sequence="4"><SpecRef id="core"/></StackupLayer>
            <StackupLayer layerOrGroupRef="FINISH" sequence="2"><SpecRef id="finish"/></StackupLayer>
          </StackupGroup>
        </Stackup>
        <Step name="board" type="BOARD"><Profile><Polygon>
          <PolyBegin x="1" y="2"/><PolyStepSegment x="2" y="2"/>
          <PolyStepSegment x="2" y="2.5"/><PolyStepSegment x="1" y="2.5"/>
          <PolyStepSegment x="1" y="2"/>
        </Polygon></Profile>
          <LayerFeature layerRef="DRILL"><Set>
            <Hole name="H1" diameter="0.025" platingStatus="PLATED" x="1.2" y="2.2"/>
          </Set></LayerFeature>
        </Step>
        <Step name="panel" type="PALLET"><Profile><Polygon>
          <PolyBegin x="0" y="0"/><PolyStepSegment x="5" y="0"/>
          <PolyStepSegment x="5" y="4"/><PolyStepSegment x="0" y="4"/>
          <PolyStepSegment x="0" y="0"/>
        </Polygon></Profile>
          <StepRepeat stepRef="board" x="0" y="0" nx="2" ny="1" dx="2" dy="0"/>
        </Step>
      </CadData></Ecad></IPC-2581>"#;

    fn job_design() -> ImportedDesign {
        import_design(
            &ipc2581::Ipc2581::parse(JOB_BOARD).unwrap(),
            Resolution::default(),
        )
        .unwrap()
    }

    fn job_value(design: &ImportedDesign, files: &[ManufacturingFile]) -> Value {
        serde_json::from_str(
            &build_gerber_job(design, ArtworkScope::Board, files)
                .unwrap()
                .contents,
        )
        .unwrap()
    }

    #[test]
    fn job_metadata_uses_ipc_units_sequence_and_explicit_finish() {
        let mut design = job_design();
        let job = job_value(&design, &[]);
        let general = &job["GeneralSpecs"];
        assert!((general["Size"]["X"].as_f64().unwrap() - 25.4).abs() < 1e-8);
        assert!((general["Size"]["Y"].as_f64().unwrap() - 12.7).abs() < 1e-8);
        assert_eq!(general["LayerNumber"], 2);
        assert!((general["BoardThickness"].as_f64().unwrap() - 1.5748).abs() < 1e-8);
        assert_eq!(general["Finish"], "ENIG");
        assert!(general.get("ImpedanceControlled").is_none());
        assert!(job.get("DesignRules").is_none());
        let materials = job["MaterialStackup"].as_array().unwrap();
        assert_eq!(
            materials
                .iter()
                .map(|m| m["Name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["MASK", "FINISH", "TOP", "CORE", "BOTTOM"]
        );
        assert!(materials[0].get("Thickness").is_none());
        assert_eq!(materials[0]["Color"], "Green");
        assert_eq!(materials[3]["Type"], "Dielectric");
        assert_eq!(materials[3]["Material"], "FR4");
        assert_eq!(materials[3]["DielectricConstant"], 4.2);
        assert_eq!(materials[3]["LossTangent"], 0.018);
        assert!((materials[2]["Thickness"].as_f64().unwrap() - 0.0254).abs() < 1e-8);
        assert!((materials[4]["Thickness"].as_f64().unwrap() - 0.0508).abs() < 1e-8);

        design.stackups[0].where_measured = Some(ipc2581::types::WhereMeasured::Mask);
        assert!(
            job_value(&design, &[])["GeneralSpecs"]
                .get("BoardThickness")
                .is_none()
        );
        design.stackups.push(design.stackups[0].clone());
        let ambiguous = job_value(&design, &[]);
        assert!(ambiguous.get("MaterialStackup").is_none());
        assert!(ambiguous["GeneralSpecs"].get("Finish").is_none());
        design.stackups.clear();
        let missing = job_value(&design, &[]);
        assert!(missing.get("MaterialStackup").is_none());
        assert!(missing["GeneralSpecs"].get("Finish").is_none());
    }

    #[test]
    fn job_preserves_kicad_material_color_properties() {
        for function in ["SOLDERMASK", "SILKSCREEN"] {
            for (property, expected) in [
                ("Black", Some("Black")),
                ("White", Some("White")),
                ("#1974AB", Some("R025G116B171")),
                ("#1974ABDD", Some("R025G116B171")),
                ("Not specified", None),
                ("#12345", None),
                ("#1974ABZZ", None),
            ] {
                let source = JOB_BOARD.replace("SOLDERMASK", function).replace(
                    r#"<ColorTerm name="GREEN"/>"#,
                    &format!(r#"<Property text="Color : {property}"/>"#),
                );
                let design = import_design(
                    &ipc2581::Ipc2581::parse(&source).unwrap(),
                    Resolution::default(),
                )
                .unwrap();
                let job = job_value(&design, &[]);
                assert_eq!(
                    job["MaterialStackup"][0]["Color"].as_str(),
                    expected,
                    "{function}: {property}"
                );
            }
        }
    }

    #[test]
    fn finish_does_not_guess_lead_content_or_choose_between_conflicting_coatings() {
        let two_coatings = JOB_BOARD
            .replace(
                "<Layer name=\"FINISH\"",
                "<Layer name=\"FINISH_BOTTOM\" layerFunction=\"COATINGCOND\" side=\"BOTTOM\"/><Layer name=\"FINISH\"",
            )
            .replace("</StackupGroup>", r#"<StackupLayer layerOrGroupRef="FINISH_BOTTOM" sequence="6"><SpecRef id="unused"/></StackupLayer></StackupGroup>"#);
        for (source, expected) in [
            (JOB_BOARD.replace("ENIG-N", "S"), Some(json!("S"))),
            (JOB_BOARD.replace("ENIG-N", "N"), Some(json!("N"))),
            (JOB_BOARD.replace("ENIG-N", "NB"), Some(json!("NB"))),
            (JOB_BOARD.replace("ENIG-N", "OTHER"), None),
            (two_coatings.clone(), None),
            (two_coatings.replace("<SpecRef id=\"unused\"/>", ""), None),
            (
                two_coatings.replace("<SurfaceFinish type=\"OSP\"/>", ""),
                None,
            ),
            (
                two_coatings.replace("<SpecRef id=\"unused\"/>", "<SpecRef id=\"missing\"/>"),
                None,
            ),
            (two_coatings.replace("OSP", "ENIG-G"), Some(json!("ENIG"))),
        ] {
            let design = import_design(
                &ipc2581::Ipc2581::parse(&source).unwrap(),
                Resolution::default(),
            )
            .unwrap();
            assert_eq!(
                job_value(&design, &[])["GeneralSpecs"].get("Finish"),
                expected.as_ref()
            );
        }
    }

    #[test]
    fn job_does_not_borrow_an_unused_boards_profile() {
        let ipc = ipc2581::Ipc2581::parse(r#"<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
          <Content roleRef="owner"><FunctionMode mode="FABRICATION"/><StepRef name="selected"/></Content>
          <Ecad><CadHeader units="MILLIMETER"/><CadData>
            <Layer name="TOP" layerFunction="SIGNAL" side="TOP"/>
            <Step name="unused" type="BOARD"><Profile><Polygon>
              <PolyBegin x="0" y="0"/><PolyStepSegment x="30" y="0"/>
              <PolyStepSegment x="30" y="10"/><PolyStepSegment x="0" y="10"/>
              <PolyStepSegment x="0" y="0"/>
            </Polygon></Profile></Step>
            <Step name="selected" type="BOARD"/>
          </CadData></Ecad></IPC-2581>"#).unwrap();
        let design = import_design(&ipc, Resolution::default()).unwrap();
        assert!(
            job_value(&design, &[])["GeneralSpecs"]
                .get("Size")
                .is_none()
        );
    }

    #[test]
    fn job_omits_incomplete_or_ambiguously_ordered_material_stacks() {
        for omitted in ["BOTTOM", "CORE", "MASK"] {
            let mut design = job_design();
            let name = design
                .layer_definitions
                .iter()
                .find(|l| design.resolve(l.name) == omitted)
                .unwrap()
                .name;
            design.stackups[0].layers.retain(|l| l.layer_ref != name);
            assert!(
                job_value(&design, &[]).get("MaterialStackup").is_none(),
                "{omitted}"
            );
        }
        let mut design = job_design();
        design.stackups[0].layers[0].layer_number = Some(1);
        assert!(job_value(&design, &[]).get("MaterialStackup").is_none());

        let mut design = job_design();
        let mut duplicate = design.stackups[0].layers[3].clone();
        duplicate.layer_number = Some(6);
        design.stackups[0].layers.push(duplicate);
        assert!(job_value(&design, &[]).get("MaterialStackup").is_none());
    }

    #[test]
    fn mixed_board_array_omits_single_board_size() {
        let source = JOB_BOARD
            .replace(
                "<Step name=\"panel\"",
                r#"<Step name="other" type="BOARD"><Profile><Polygon>
              <PolyBegin x="0" y="0"/><PolyStepSegment x="3" y="0"/>
              <PolyStepSegment x="3" y="1"/><PolyStepSegment x="0" y="1"/>
              <PolyStepSegment x="0" y="0"/>
            </Polygon></Profile></Step><Step name="panel""#,
            )
            .replace(
                "<StepRepeat stepRef=\"board\"",
                "<StepRepeat stepRef=\"other\" x=\"0\" y=\"2\"/><StepRepeat stepRef=\"board\"",
            );
        let design = import_design(
            &ipc2581::Ipc2581::parse(&source).unwrap(),
            Resolution::default(),
        )
        .unwrap();
        for view in [ArtworkScope::Board, ArtworkScope::ArrayFlattened] {
            let package = build_manufacturing_package(
                &design,
                &ManufacturingExportOptions {
                    view,
                    include_auxiliary_layers: false,
                    relief_debug_dir: None,
                },
                Resolution::default(),
            )
            .unwrap();
            let job: Value = serde_json::from_str(&package.files.last().unwrap().contents).unwrap();
            if view == ArtworkScope::Board {
                assert!((job["GeneralSpecs"]["Size"]["X"].as_f64().unwrap() - 76.2).abs() < 1e-8);
            } else {
                assert!(job["GeneralSpecs"].get("Size").is_none());
            }
        }
    }

    #[test]
    fn job_inventory_follows_emitted_names_attributes_and_auxiliary_files() {
        let files = [
            ManufacturingFile {
                filename: "renamed-inner.g1".into(),
                contents: "%TF.FileFunction,Copper,L2,Inr*%\n%TF.FilePolarity,Positive*%\n".into(),
            },
            ManufacturingFile {
                filename: "mask.gts".into(),
                contents: "%TF.FileFunction,Soldermask,Top*%\n%TF.FilePolarity,Negative*%\n".into(),
            },
            ManufacturingFile {
                filename: "optional.gbr".into(),
                contents: "%TF.FileFunction,Other,Fab*%\n%TF.FilePolarity,Positive*%\n".into(),
            },
            ManufacturingFile {
                filename: "blind.drl".into(),
                contents: "M48\n; #@! TF.FileFunction,Plated,1,2,Blind\n".into(),
            },
        ];
        assert_eq!(
            job_value(&job_design(), &files)["FilesAttributes"],
            json!([
                {"Path":"renamed-inner.g1", "FileFunction":"Copper,L2,Inr", "FilePolarity":"Positive", "FileFormat":"Gerber"},
                {"Path":"mask.gts", "FileFunction":"Soldermask,Top", "FilePolarity":"Negative", "FileFormat":"Gerber"},
                {"Path":"optional.gbr", "FileFunction":"Other,Fab", "FilePolarity":"Positive", "FileFormat":"Gerber"},
                {"Path":"blind.drl", "FileFunction":"Plated,1,2,Blind", "FileFormat":"XNC"},
            ])
        );
        assert_eq!(
            job_value(&job_design(), &files[..2])["FilesAttributes"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn board_and_array_packages_include_job_with_exact_inventory_and_single_board_size() {
        let design = job_design();
        let mut board_size = None;
        for view in [ArtworkScope::Board, ArtworkScope::ArrayFlattened] {
            let package = build_manufacturing_package(
                &design,
                &ManufacturingExportOptions {
                    view,
                    include_auxiliary_layers: false,
                    relief_debug_dir: None,
                },
                Resolution::default(),
            )
            .unwrap();
            let job_file = package
                .files
                .iter()
                .find(|file| file.filename == "job.gbrjob")
                .unwrap();
            let job: Value = serde_json::from_str(&job_file.contents).unwrap();
            let entries = job["FilesAttributes"].as_array().unwrap();
            assert_eq!(entries.len(), package.files.len() - 1);
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry["Path"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                package
                    .files
                    .iter()
                    .filter(|file| file.filename != "job.gbrjob")
                    .map(|file| file.filename.as_str())
                    .collect::<Vec<_>>()
            );
            assert!(
                entries
                    .iter()
                    .all(|entry| entry["FileFunction"].is_string())
            );
            let drill = entries
                .iter()
                .find(|entry| entry["Path"] == "PTH.drl")
                .unwrap();
            assert_eq!(drill["FileFormat"], "XNC");
            assert_eq!(drill["FileFunction"], "Plated,1,2,PTH");
            assert!(drill.get("FilePolarity").is_none());
            if let Some(size) = &board_size {
                assert_eq!(&job["GeneralSpecs"]["Size"], size);
            }
            board_size = Some(job["GeneralSpecs"]["Size"].clone());
            let mut archive = zip::ZipArchive::new(Cursor::new(package.to_zip().unwrap())).unwrap();
            assert_eq!(archive.len(), package.files.len());
            for file in &package.files {
                let mut restored = String::new();
                archive
                    .by_name(&file.filename)
                    .unwrap()
                    .read_to_string(&mut restored)
                    .unwrap();
                assert_eq!(restored, file.contents);
            }
            #[cfg(feature = "cli")]
            {
                let dir = tempfile::tempdir().unwrap();
                write_manufacturing_package(&package, dir.path()).unwrap();
                write_manufacturing_package(&package, dir.path()).unwrap();
                assert_eq!(
                    fs::read_dir(dir.path()).unwrap().count(),
                    package.files.len()
                );
                for file in &package.files {
                    assert_eq!(
                        fs::read_to_string(dir.path().join(&file.filename)).unwrap(),
                        file.contents
                    );
                }
            }
        }
    }

    #[cfg(feature = "cli")]
    #[test]
    fn directory_rejects_stale_auxiliary_files_without_changing_existing_files() {
        for name in [
            "F_Fab.gbr",
            "F_Adhesive.gta",
            "B_Adhesive.gba",
            "Custom.GBR",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join("gerbers");
            let mut package = ManufacturingPackage {
                files: vec![
                    ManufacturingFile {
                        filename: "F_Cu.gtl".into(),
                        contents: "old copper".into(),
                    },
                    ManufacturingFile {
                        filename: name.into(),
                        contents: "drawing".into(),
                    },
                ],
            };
            write_manufacturing_package(&package, &output).unwrap();
            fs::write(output.join("notes.txt"), "keep me").unwrap();
            // Re-exporting the same file set remains supported.
            write_manufacturing_package(&package, &output).unwrap();
            let zip = directory.path().join("gerbers.zip");
            write_manufacturing_package(&package, &zip).unwrap();
            package.files.pop();
            package.files[0].contents = "new copper".into();
            let error = write_manufacturing_package(&package, &output).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("use a fresh directory or a ZIP output")
            );
            assert_eq!(
                fs::read_to_string(output.join("F_Cu.gtl")).unwrap(),
                "old copper"
            );
            assert_eq!(fs::read_to_string(output.join(name)).unwrap(), "drawing");
            assert_eq!(
                fs::read_to_string(output.join("notes.txt")).unwrap(),
                "keep me"
            );
            // ZIP replacement does remove the old entry.
            write_manufacturing_package(&package, &zip).unwrap();
            let mut archive = zip::ZipArchive::new(fs::File::open(zip).unwrap()).unwrap();
            assert_eq!(archive.len(), 1);
            assert_eq!(archive.by_index(0).unwrap().name(), "F_Cu.gtl");
            fs::remove_file(output.join(name)).unwrap();
            write_manufacturing_package(&package, &output).unwrap();
            assert_eq!(
                fs::read_to_string(output.join("F_Cu.gtl")).unwrap(),
                "new copper"
            );
        }
    }

    #[test]
    fn in_memory_zip_preserves_every_filename_and_contents() {
        let package = ManufacturingPackage {
            files: vec![
                ManufacturingFile {
                    filename: "PTH.drl".to_owned(),
                    contents: "M48\nMETRIC\nT01C0.6\n%\nT01\nX1.0Y2.0\nM30\n".to_owned(),
                },
                ManufacturingFile {
                    filename: "NPTH.drl".to_owned(),
                    contents: "M48\nMETRIC\nT01C2.0\n%\nT01\nX3.0Y4.0\nM30\n".to_owned(),
                },
            ],
        };
        let zipped = package.to_zip().unwrap();
        assert_eq!(zipped, package.to_zip().unwrap());
        let mut archive = zip::ZipArchive::new(Cursor::new(zipped)).unwrap();
        assert_eq!(archive.len(), package.files.len());
        for file in &package.files {
            let mut restored = String::new();
            archive
                .by_name(&file.filename)
                .unwrap()
                .read_to_string(&mut restored)
                .unwrap();
            assert_eq!(restored, file.contents);
        }
    }
}
