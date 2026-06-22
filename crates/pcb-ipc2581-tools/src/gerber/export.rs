use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gerberx2::{GerberLayer, write_layer};
use ipc2581::Ipc2581;
use ipc2581::types::{LayerFunction, Side, ecad::Layer};

use super::artwork::{ArtworkLayer, LayerAttributes, ObjectAttributes};
use super::lower::lower_artwork_layer;
use crate::{LayoutTarget, geometry, ipc2581 as ipc};
use pcb_ir::common::{BBox, LayerRole, LineCap, LineJoin, PaintPolarity, Unit};
use pcb_ir::dialects::artwork::{ArtworkGeometry, ArtworkObject, ArtworkPath};
use pcb_ir::dialects::ipc::{
    FeatureBucket, FeatureSemantic, FiducialKind, GeometryFeature, GeometryPath,
};
use pcb_ir::dialects::path as common_path;

#[derive(Debug, Clone)]
pub struct GerberExportOptions {
    pub output_dir: PathBuf,
    pub layout_target: LayoutTarget,
}

#[derive(Debug, Clone)]
pub struct GerberExportSet {
    pub files: Vec<GerberExportFile>,
}

#[derive(Debug, Clone)]
pub struct GerberExportFile {
    pub filename: String,
    pub layer: GerberLayer,
    pub contents: String,
}

pub fn export_gerber_x2(ipc: &Ipc2581, options: &GerberExportOptions) -> Result<GerberExportSet> {
    if options.layout_target == LayoutTarget::Layout {
        bail!("Gerber export does not support --layout-target layout; use board or panel");
    }

    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut files = Vec::new();
    let mut first_doc = None;
    let plans = export_layer_plans(&ecad.cad_data.layers);

    for plan in &plans {
        let source_layer = plan.layer;
        let layer_name = ipc.resolve(source_layer.name);
        let mut doc =
            geometry::extract_layer_for_layout_target(ipc, layer_name, options.layout_target)
                .with_context(|| format!("failed to extract IPC-2581 layer '{layer_name}'"))?;
        pcb_ir::dialects::ipc::process::process_document(&mut doc);
        if first_doc.is_none() {
            first_doc = Some(doc.clone());
        }
        let artwork = artwork_from_processed_layer(
            ipc,
            &doc,
            0,
            plan.role.ir_role(),
            layer_attributes(plan.file_function.clone(), options.layout_target),
        )?;
        let layer = lower_artwork_layer(&artwork)?;
        let contents = write_layer(&layer)?;
        files.push(GerberExportFile {
            filename: plan.filename.clone(),
            layer,
            contents,
        });
    }

    if let Some(doc) = first_doc {
        let profile = profile_artwork_from_profiles(&doc, options.layout_target)?;
        if !profile.objects.is_empty() {
            let layer = lower_artwork_layer(&profile)?;
            let contents = write_layer(&layer)?;
            files.push(GerberExportFile {
                filename: "profile.gbr".to_string(),
                layer,
                contents,
            });
        }
    }

    std::fs::create_dir_all(&options.output_dir).with_context(|| {
        format!(
            "failed to create Gerber output directory {}",
            options.output_dir.display()
        )
    })?;
    for file in &files {
        std::fs::write(options.output_dir.join(&file.filename), &file.contents).with_context(
            || {
                format!(
                    "failed to write Gerber file {}",
                    options.output_dir.join(&file.filename).display()
                )
            },
        )?;
    }

    Ok(GerberExportSet { files })
}

struct ExportLayerPlan<'a> {
    layer: &'a Layer,
    role: GerberLayerRole,
    filename: String,
    file_function: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GerberLayerRole {
    Copper,
    Paste,
    Soldermask,
    Legend,
    Profile,
    Vcut,
    Score,
}

fn export_layer_plans(layers: &[Layer]) -> Vec<ExportLayerPlan<'_>> {
    let copper_count = layers
        .iter()
        .filter(|layer| gerber_layer_role(layer.layer_function) == Some(GerberLayerRole::Copper))
        .count();
    let mut copper_index = 0;
    let mut plans = Vec::new();

    for layer in layers {
        let Some(role) = gerber_layer_role(layer.layer_function) else {
            continue;
        };
        if role == GerberLayerRole::Copper {
            copper_index += 1;
        }
        let (filename, file_function) = layer_output(role, layer.side, copper_index, copper_count);
        plans.push(ExportLayerPlan {
            layer,
            role,
            filename,
            file_function,
        });
    }

    plans
}

fn gerber_layer_role(function: LayerFunction) -> Option<GerberLayerRole> {
    match function {
        LayerFunction::Conductor
        | LayerFunction::CondFilm
        | LayerFunction::CondFoil
        | LayerFunction::Plane
        | LayerFunction::Signal
        | LayerFunction::Mixed => Some(GerberLayerRole::Copper),
        LayerFunction::Solderpaste | LayerFunction::Pastemask => Some(GerberLayerRole::Paste),
        LayerFunction::Soldermask => Some(GerberLayerRole::Soldermask),
        LayerFunction::Silkscreen | LayerFunction::Legend => Some(GerberLayerRole::Legend),
        LayerFunction::Rout | LayerFunction::BoardOutline => Some(GerberLayerRole::Profile),
        LayerFunction::VCut => Some(GerberLayerRole::Vcut),
        LayerFunction::Score => Some(GerberLayerRole::Score),
        _ => None,
    }
}

impl GerberLayerRole {
    fn ir_role(self) -> LayerRole {
        match self {
            GerberLayerRole::Copper => LayerRole::Copper,
            GerberLayerRole::Paste => LayerRole::Paste,
            GerberLayerRole::Soldermask => LayerRole::Soldermask,
            GerberLayerRole::Legend => LayerRole::Legend,
            GerberLayerRole::Profile | GerberLayerRole::Vcut | GerberLayerRole::Score => {
                LayerRole::Profile
            }
        }
    }
}

fn layer_output(
    role: GerberLayerRole,
    side: Option<Side>,
    copper_index: usize,
    copper_count: usize,
) -> (String, Vec<String>) {
    match role {
        GerberLayerRole::Copper => copper_layer_output(side, copper_index, copper_count),
        GerberLayerRole::Paste => match side {
            Some(Side::Bottom) => (
                "B_Paste.gbp".to_string(),
                vec!["Paste".into(), "Bot".into()],
            ),
            _ => (
                "F_Paste.gtp".to_string(),
                vec!["Paste".into(), "Top".into()],
            ),
        },
        GerberLayerRole::Soldermask => match side {
            Some(Side::Bottom) => (
                "B_Mask.gbs".to_string(),
                vec!["Soldermask".into(), "Bot".into()],
            ),
            _ => (
                "F_Mask.gts".to_string(),
                vec!["Soldermask".into(), "Top".into()],
            ),
        },
        GerberLayerRole::Legend => match side {
            Some(Side::Bottom) => (
                "B_SilkS.gbo".to_string(),
                vec!["Legend".into(), "Bot".into()],
            ),
            _ => (
                "F_SilkS.gto".to_string(),
                vec!["Legend".into(), "Top".into()],
            ),
        },
        GerberLayerRole::Profile => (
            "Edge_Cuts.gm1".to_string(),
            vec!["Profile".into(), "NP".into()],
        ),
        GerberLayerRole::Vcut => fabrication_line_layer_output("V_Cut.gbr", &["Vcut"], side),
        GerberLayerRole::Score => {
            fabrication_line_layer_output("Score.gbr", &["Other", "Score"], side)
        }
    }
}

fn fabrication_line_layer_output(
    filename: &str,
    function: &[&str],
    side: Option<Side>,
) -> (String, Vec<String>) {
    let mut file_function = function
        .iter()
        .map(|field| (*field).to_string())
        .collect::<Vec<_>>();
    match side {
        Some(Side::Top) => file_function.push("Top".to_string()),
        Some(Side::Bottom) => file_function.push("Bot".to_string()),
        Some(Side::Both) | Some(Side::All) => file_function.push("Top/Bot".to_string()),
        _ => {}
    }
    (filename.to_string(), file_function)
}

fn layer_attributes(file_function: Vec<String>, layout_target: LayoutTarget) -> LayerAttributes {
    LayerAttributes {
        file_function,
        part: Some(vec![match layout_target {
            LayoutTarget::Board => "Single".to_string(),
            LayoutTarget::Panel | LayoutTarget::Layout => "Array".to_string(),
        }]),
        file_polarity: None,
    }
}

fn copper_layer_output(
    side: Option<Side>,
    copper_index: usize,
    copper_count: usize,
) -> (String, Vec<String>) {
    let side_field = match side {
        Some(Side::Top) => "Top",
        Some(Side::Bottom) => "Bot",
        _ => "Inr",
    };
    let filename = match side {
        Some(Side::Top) => "F_Cu.gtl".to_string(),
        Some(Side::Bottom) => "B_Cu.gbl".to_string(),
        _ => format!("In{}_Cu.gbr", copper_index),
    };
    let index = match side {
        Some(Side::Top) => 1,
        Some(Side::Bottom) => copper_count,
        _ => copper_index,
    };
    (
        filename,
        vec![
            "Copper".to_string(),
            format!("L{index}"),
            side_field.to_string(),
        ],
    )
}

fn artwork_from_processed_layer(
    ipc: &Ipc2581,
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    layer_index: usize,
    role: LayerRole,
    meta: LayerAttributes,
) -> Result<ArtworkLayer> {
    let layer = &doc.layers[layer_index];
    let mut artwork = ArtworkLayer::new(Unit::Millimeter);
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::ArtworkLayer {
        name: layer.name.clone(),
        role,
        side: pcb_ir::common::Side::None,
        object_start: 0,
        object_count: 0,
        bbox: layer.bbox,
        meta,
    });
    for feature in &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
    {
        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            let aperture_function = path.flags.stroked.then(|| aperture_function(feature));
            push_artwork_object(
                &mut artwork,
                artwork_layer,
                doc,
                path,
                object_attributes(ipc, doc, feature, aperture_function),
                &layer.name,
            )?;
        }
    }
    Ok(artwork)
}

const PROFILE_STROKE_WIDTH: f64 = 0.1;

fn profile_artwork_from_profiles(
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    layout_target: LayoutTarget,
) -> Result<ArtworkLayer> {
    let mut artwork = ArtworkLayer::new(Unit::Millimeter);
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::ArtworkLayer {
        name: "Profile".to_string(),
        role: LayerRole::Profile,
        side: pcb_ir::common::Side::None,
        object_start: 0,
        object_count: 0,
        bbox: BBox::empty(),
        meta: layer_attributes(vec!["Profile".into(), "NP".into()], layout_target),
    });
    for occurrence in
        pcb_ir::dialects::ipc::profile_occurrences_for(doc, layout_target.profile_set())
    {
        push_profile_artwork_object(
            &mut artwork,
            artwork_layer,
            doc,
            occurrence.profile.outer_path,
            occurrence.transform,
        );
        for cutout in &doc.profile_cutouts[occurrence.profile.cutout_start as usize
            ..(occurrence.profile.cutout_start + occurrence.profile.cutout_count) as usize]
        {
            push_profile_artwork_object(
                &mut artwork,
                artwork_layer,
                doc,
                cutout.path,
                occurrence.transform,
            );
        }
    }
    Ok(artwork)
}

fn object_attributes(
    ipc: &Ipc2581,
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    feature: &GeometryFeature<ipc2581::Symbol>,
    aperture_function: Option<Vec<String>>,
) -> ObjectAttributes {
    let pin_ref = (feature.pin_ref_count > 0)
        .then(|| doc.pin_refs.get(feature.pin_ref_start as usize))
        .flatten();
    ObjectAttributes {
        aperture_function,
        net: feature.net.map(|symbol| ipc.resolve(symbol).to_string()),
        component: pin_ref
            .and_then(|pin_ref| pin_ref.component_ref)
            .map(|symbol| ipc.resolve(symbol).to_string()),
        pin: pin_ref.map(|pin_ref| ipc.resolve(pin_ref.pin).to_string()),
    }
}

fn aperture_function(feature: &GeometryFeature<ipc2581::Symbol>) -> Vec<String> {
    match feature.semantic {
        FeatureSemantic::Fiducial(kind) => {
            let kind = match kind {
                FiducialKind::Local => "Local",
                FiducialKind::Global => "Global",
                FiducialKind::Panel | FiducialKind::GoodPanel => "Panel",
                FiducialKind::BadBoard => {
                    return vec!["Other".to_string(), "BadBoardMark".to_string()];
                }
            };
            vec!["FiducialPad".to_string(), kind.to_string()]
        }
        FeatureSemantic::SmdPad => vec!["SMDPad".to_string()],
        FeatureSemantic::ComponentPad => vec!["ComponentPad".to_string()],
        FeatureSemantic::ViaPad => vec!["ViaPad".to_string()],
        FeatureSemantic::VCut => vec!["Other".to_string(), "Vcut".to_string()],
        FeatureSemantic::Score => vec!["Other".to_string(), "Score".to_string()],
        FeatureSemantic::Route | FeatureSemantic::BoardOutline => vec!["Profile".to_string()],
        _ => match feature.bucket {
            FeatureBucket::Smd => vec!["SMDPad".to_string()],
            FeatureBucket::Pth => vec!["ComponentPad".to_string()],
            FeatureBucket::Via => vec!["ViaPad".to_string()],
            FeatureBucket::Fiducial => vec!["FiducialPad".to_string()],
            FeatureBucket::Trace | FeatureBucket::Fill => vec!["Conductor".to_string()],
            FeatureBucket::Cutout => vec!["Other".to_string()],
            FeatureBucket::Thermal => vec!["ThermalRelief".to_string()],
            FeatureBucket::Antipad => vec!["AntiPad".to_string()],
        },
    }
}

fn push_artwork_path(
    artwork: &mut ArtworkLayer,
    artwork_path: ArtworkPath,
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    path: &GeometryPath,
) -> u32 {
    artwork.push_path(artwork_path, artwork_contours(doc, path))
}

fn push_artwork_object(
    artwork: &mut ArtworkLayer,
    artwork_layer: u32,
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    path: &GeometryPath,
    meta: ObjectAttributes,
    layer_name: &str,
) -> Result<()> {
    let (geometry, path_id) = if path.flags.filled {
        let path = push_artwork_path(artwork, ArtworkPath::filled(path.fill_rule), doc, path);
        (ArtworkGeometry::Region { path }, path)
    } else if path.flags.stroked {
        let artwork_path = ArtworkPath::stroked(path.stroke_width, path.line_cap, LineJoin::Round);
        let path = push_artwork_path(artwork, artwork_path, doc, path);
        (ArtworkGeometry::Stroke { path }, path)
    } else {
        bail!("processed IPC geometry path is neither filled nor stroked on layer '{layer_name}'");
    };
    artwork.push_object(
        artwork_layer,
        ArtworkObject {
            paint: PaintPolarity::Dark,
            geometry,
            net: None,
            bbox: artwork.paths[path_id as usize].bbox,
            meta,
        },
    );
    Ok(())
}

fn push_profile_artwork_object(
    artwork: &mut ArtworkLayer,
    artwork_layer: u32,
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    path_index: u32,
    transform: pcb_ir::common::Affine2,
) {
    let artwork_path = ArtworkPath::stroked(PROFILE_STROKE_WIDTH, LineCap::Round, LineJoin::Round);
    let path_id = artwork.push_path(
        artwork_path,
        pcb_ir::dialects::ipc::transformed_path_payloads(doc, path_index, transform),
    );
    artwork.push_object(
        artwork_layer,
        ArtworkObject {
            paint: PaintPolarity::Dark,
            geometry: ArtworkGeometry::Stroke { path: path_id },
            net: None,
            bbox: artwork.paths[path_id as usize].bbox,
            meta: ObjectAttributes {
                aperture_function: Some(vec!["Profile".to_string()]),
                net: None,
                component: None,
                pin: None,
            },
        },
    );
}

fn artwork_contours(
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    path: &GeometryPath,
) -> Vec<common_path::PathPayload> {
    doc.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| common_path::PathPayload {
            bbox: contour.bbox,
            cmds: doc.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

pub fn execute_file(input_file: &Path, output_dir: &Path) -> Result<GerberExportSet> {
    execute_file_with_options(
        input_file,
        &GerberExportOptions {
            output_dir: output_dir.to_path_buf(),
            layout_target: LayoutTarget::Board,
        },
    )
}

pub fn execute_file_with_options(
    input_file: &Path,
    options: &GerberExportOptions,
) -> Result<GerberExportSet> {
    let content = crate::utils::file::load_ipc_file(input_file)?;
    let ipc = ipc::Ipc2581::parse(&content)?;
    export_gerber_x2(&ipc, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn exports_ipc_layer_to_parseable_gerber_x2() {
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="board"/>
    <LayerRef name="TOP"/>
    <DictionaryStandard units="MILLIMETER">
      <EntryStandard id="pad"><Circle diameter="1"/></EntryStandard>
    </DictionaryStandard>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER"/>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Step name="board" type="BOARD">
        <Profile>
          <Polygon>
            <PolyBegin x="0" y="0"/>
            <PolyStepSegment x="10" y="0"/>
            <PolyStepSegment x="10" y="10"/>
            <PolyStepSegment x="0" y="0"/>
          </Polygon>
        </Profile>
        <PadStackDef name="padstack">
          <PadstackPadDef layerRef="TOP" padUse="REGULAR">
            <StandardPrimitiveRef id="pad"/>
          </PadstackPadDef>
        </PadStackDef>
        <LayerFeature layerRef="TOP">
          <Set net="N1">
            <Pad padstackDefRef="padstack"><Location x="2" y="3"/></Pad>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let output_dir =
            std::env::temp_dir().join(format!("pcb-ipc-gerber-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&output_dir);

        let set = export_gerber_x2(
            &ipc,
            &GerberExportOptions {
                output_dir,
                layout_target: LayoutTarget::Board,
            },
        )
        .unwrap();

        assert!(set.files.iter().any(|file| file.filename == "F_Cu.gtl"));
        assert!(set.files.iter().any(|file| file.filename == "profile.gbr"));
        for file in &set.files {
            gerberx2::GerberX2::parse(&file.contents).unwrap();
        }
        let copper = set
            .files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(copper.contents.contains("%TF.FileFunction,Copper,L1,Top*%"));
        assert!(copper.contents.contains("%TO.N,N1*%"));
    }

    #[test]
    fn gerber_export_rejects_symbolic_layout_target() {
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
  </Content>
</IPC-2581>"#,
        )
        .unwrap();
        let output_dir =
            std::env::temp_dir().join(format!("pcb-ipc-gerber-layout-test-{}", std::process::id()));

        let error = export_gerber_x2(
            &ipc,
            &GerberExportOptions {
                output_dir,
                layout_target: LayoutTarget::Layout,
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("Gerber export does not support --layout-target layout")
        );
    }

    #[test]
    fn gerber_export_carries_vcut_and_fiducial_x2_metadata() {
        let ipc = ipc::Ipc2581::parse(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="owner">
    <FunctionMode mode="FABRICATION"/>
    <StepRef name="Panel"/>
    <LayerRef name="TOP"/>
    <LayerRef name="VCUT"/>
    <LayerRef name="SCORE"/>
    <DictionaryLineDesc units="MILLIMETER">
      <EntryLineDesc id="fidline">
        <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
      </EntryLineDesc>
    </DictionaryLineDesc>
  </Content>
  <Ecad>
    <CadHeader units="MILLIMETER">
      <Spec name="VCut_1">
        <V_Cut type="ANGLE">
          <Property value="90" unit="DEGREES"/>
        </V_Cut>
      </Spec>
    </CadHeader>
    <CadData>
      <Layer name="TOP" layerFunction="SIGNAL" side="TOP" polarity="POSITIVE"/>
      <Layer name="VCUT" layerFunction="V_CUT" side="ALL" polarity="POSITIVE">
        <SpecRef id="VCut_1"/>
      </Layer>
      <Layer name="SCORE" layerFunction="SCORE" side="ALL" polarity="POSITIVE"/>
      <Step name="Panel" type="PALLET">
        <LayerFeature layerRef="TOP">
          <Set>
            <GlobalFiducial>
              <Location x="1" y="2"/>
              <Circle diameter="1">
                <FillDesc fillProperty="HOLLOW"/>
                <LineDescRef id="fidline"/>
              </Circle>
              <PinRef componentRef="U1" pin="1"/>
            </GlobalFiducial>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="VCUT">
          <Set>
            <Features>
              <Line startX="0" startY="5" endX="10" endY="5">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
        <LayerFeature layerRef="SCORE">
          <Set>
            <Features>
              <Line startX="0" startY="7" endX="10" endY="7">
                <LineDesc lineWidth="0.1" lineEnd="ROUND"/>
              </Line>
            </Features>
          </Set>
        </LayerFeature>
      </Step>
    </CadData>
  </Ecad>
</IPC-2581>"#,
        )
        .unwrap();
        let output_dir =
            std::env::temp_dir().join(format!("pcb-ipc-gerber-x2-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&output_dir);

        let set = export_gerber_x2(
            &ipc,
            &GerberExportOptions {
                output_dir,
                layout_target: LayoutTarget::Panel,
            },
        )
        .unwrap();

        let top = set
            .files
            .iter()
            .find(|file| file.filename == "F_Cu.gtl")
            .unwrap();
        assert!(top.contents.contains("%TF.Part,Array*%"));
        assert!(
            top.contents
                .contains("%TA.AperFunction,FiducialPad,Global*%")
        );
        assert!(top.contents.contains("%TO.C,U1*%"));
        assert!(top.contents.contains("%TO.P,1*%"));

        let vcut = set
            .files
            .iter()
            .find(|file| file.filename == "V_Cut.gbr")
            .unwrap();
        assert!(vcut.contents.contains("%TF.FileFunction,Vcut,Top/Bot*%"));
        assert!(vcut.contents.contains("%TF.Part,Array*%"));
        assert!(vcut.contents.contains("%TA.AperFunction,Other,Vcut*%"));

        let score = set
            .files
            .iter()
            .find(|file| file.filename == "Score.gbr")
            .unwrap();
        assert!(
            score
                .contents
                .contains("%TF.FileFunction,Other,Score,Top/Bot*%")
        );
        assert!(score.contents.contains("%TF.Part,Array*%"));
        assert!(score.contents.contains("%TA.AperFunction,Other,Score*%"));
        assert!(!score.contents.contains("Vcut"));
    }

    #[test]
    fn real_board_export_parseback_and_svg_paths_smoke() {
        let compressed = include_bytes!("../../../ipc2581/tests/data/DM0002-IPC-2518.xml.zst");
        let content = zstd::decode_all(Cursor::new(compressed)).unwrap();
        let ipc = ipc::Ipc2581::parse(std::str::from_utf8(&content).unwrap()).unwrap();
        let output_dir =
            std::env::temp_dir().join(format!("pcb-ipc-gerber-real-smoke-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&output_dir);

        let set = export_gerber_x2(
            &ipc,
            &GerberExportOptions {
                output_dir: output_dir.clone(),
                layout_target: LayoutTarget::Board,
            },
        )
        .unwrap();

        assert!(set.files.len() >= 10);
        assert!(set.files.iter().any(|file| file.filename == "F_Cu.gtl"));
        assert!(
            set.files
                .iter()
                .any(|file| file.filename == "Edge_Cuts.gm1")
        );

        for file in &set.files {
            let parsed = gerberx2::GerberX2::parse(&file.contents).unwrap();
            let mut geometry = gerberx2::geometry::extract_document(&parsed);
            pcb_ir::dialects::gerber::process::process_document(&mut geometry);
            let svg = pcb_ir::dialects::gerber::svg::render_svg(&geometry);
            assert!(svg.contains("<svg"), "{} did not render SVG", file.filename);

            let geom = pcb_ir::dialects::gerber::lower_to_geom(&geometry);
            geom.validate().unwrap();
            let mask = pcb_ir::dialects::geom::lower_filled_to_mask(&geom);
            mask.validate().unwrap();
        }

        let mut layer = geometry::extract_layer(&ipc, "F.Cu").unwrap();
        pcb_ir::dialects::ipc::process::process_document(&mut layer);
        let geom = pcb_ir::dialects::ipc::lower_layer_to_geom(
            &layer,
            0,
            LayerRole::Copper,
            pcb_ir::common::Side::Top,
        );
        geom.validate().unwrap();
        let mask = pcb_ir::dialects::geom::lower_filled_to_mask(&geom);
        mask.validate().unwrap();
        assert!(pcb_ir::dialects::mask::render_svg(&mask, 0).contains("<svg"));

        pcb_ir::dialects::ipc::process::flatten_layers_to_masks(&mut layer);
        let flat_geom = pcb_ir::dialects::ipc::lower_layer_to_geom(
            &layer,
            0,
            LayerRole::Copper,
            pcb_ir::common::Side::Top,
        );
        flat_geom.validate().unwrap();
        let flat_mask = pcb_ir::dialects::geom::lower_filled_to_mask(&flat_geom);
        flat_mask.validate().unwrap();
        assert!(pcb_ir::dialects::mask::render_svg(&flat_mask, 0).contains("<svg"));

        let _ = std::fs::remove_dir_all(&output_dir);
    }
}
