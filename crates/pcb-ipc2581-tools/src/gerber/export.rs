use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gerberx2::{GerberLayer, write_layer};
use ipc2581::Ipc2581;
use ipc2581::types::{LayerFunction, Side, ecad::Layer};

use super::artwork::{ArtworkLayer, ObjectAttributes};
use super::lower::lower_artwork_layer;
use crate::{geometry, ipc2581 as ipc};
use pcb_ir::common::{BBox, LayerRole, LineCap, LineJoin, PaintPolarity, Unit};
use pcb_ir::dialects::artwork::{ArtworkGeometry, ArtworkObject, ArtworkPath};
use pcb_ir::dialects::ipc::{FeatureBucket, GeometryPath};
use pcb_ir::dialects::path as common_path;

#[derive(Debug, Clone)]
pub struct GerberExportOptions {
    pub output_dir: PathBuf,
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
    let ecad = ipc.ecad().context("IPC-2581 file has no ECAD section")?;
    let mut files = Vec::new();
    let mut first_doc = None;
    let plans = export_layer_plans(&ecad.cad_data.layers);

    for plan in &plans {
        let source_layer = plan.layer;
        let layer_name = ipc.resolve(source_layer.name);
        let mut doc = geometry::extract_layer(ipc, layer_name)
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
            plan.file_function.clone(),
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
        let profile = profile_artwork_from_profiles(&doc)?;
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
            GerberLayerRole::Profile => LayerRole::Profile,
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
    file_function: Vec<String>,
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
        meta: file_function,
    });
    for feature in &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
    {
        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            let aperture_function = path
                .flags
                .stroked
                .then(|| aperture_function(feature.bucket).to_string());
            push_artwork_object(
                &mut artwork,
                artwork_layer,
                doc,
                path,
                object_attributes(ipc, feature.net, aperture_function),
                &layer.name,
            )?;
        }
    }
    Ok(artwork)
}

const PROFILE_STROKE_WIDTH: f64 = 0.1;

fn profile_artwork_from_profiles(
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
) -> Result<ArtworkLayer> {
    let mut artwork = ArtworkLayer::new(Unit::Millimeter);
    let artwork_layer = artwork.push_layer(pcb_ir::dialects::artwork::ArtworkLayer {
        name: "Profile".to_string(),
        role: LayerRole::Profile,
        side: pcb_ir::common::Side::None,
        object_start: 0,
        object_count: 0,
        bbox: BBox::empty(),
        meta: vec!["Profile".into(), "NP".into()],
    });
    for profile in pcb_ir::dialects::ipc::render_profiles(doc) {
        push_profile_artwork_object(&mut artwork, artwork_layer, doc, profile.outer_path);
        for cutout in &doc.profile_cutouts
            [profile.cutout_start as usize..(profile.cutout_start + profile.cutout_count) as usize]
        {
            push_profile_artwork_object(&mut artwork, artwork_layer, doc, cutout.path);
        }
    }
    Ok(artwork)
}

fn object_attributes(
    ipc: &Ipc2581,
    net: Option<ipc2581::Symbol>,
    aperture_function: Option<String>,
) -> ObjectAttributes {
    ObjectAttributes {
        aperture_function,
        net: net.map(|symbol| ipc.resolve(symbol).to_string()),
    }
}

fn aperture_function(bucket: FeatureBucket) -> &'static str {
    match bucket {
        FeatureBucket::Smd => "SMDPad",
        FeatureBucket::Pth => "ComponentPad",
        FeatureBucket::Via => "ViaPad",
        FeatureBucket::Trace => "Conductor",
        FeatureBucket::Fill => "Conductor",
        FeatureBucket::Cutout => "Other",
        FeatureBucket::Thermal => "ThermalRelief",
        FeatureBucket::Antipad => "AntiPad",
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
) {
    let path = &doc.paths[path_index as usize];
    let artwork_path = ArtworkPath::stroked(PROFILE_STROKE_WIDTH, LineCap::Round, LineJoin::Round);
    let path_id = push_artwork_path(artwork, artwork_path, doc, path);
    artwork.push_object(
        artwork_layer,
        ArtworkObject {
            paint: PaintPolarity::Dark,
            geometry: ArtworkGeometry::Stroke { path: path_id },
            net: None,
            bbox: artwork.paths[path_id as usize].bbox,
            meta: ObjectAttributes {
                aperture_function: Some("Profile".to_string()),
                net: None,
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
    let content = crate::utils::file::load_ipc_file(input_file)?;
    let ipc = ipc::Ipc2581::parse(&content)?;
    export_gerber_x2(
        &ipc,
        &GerberExportOptions {
            output_dir: output_dir.to_path_buf(),
        },
    )
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

        let set = export_gerber_x2(&ipc, &GerberExportOptions { output_dir }).unwrap();

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
