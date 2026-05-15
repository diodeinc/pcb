use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gerberx2::{GerberLayer, write_layer};
use ipc2581::Ipc2581;
use ipc2581::types::{LayerFunction, Side, ecad::Layer};

use super::artwork::{
    ArtworkContour, ArtworkLayer, ArtworkObject, ArtworkSegment, ObjectAttributes,
};
use super::lower::lower_artwork_layer;
use crate::{geometry, ipc2581 as ipc};
use pcb_ir::common::Point;
use pcb_ir::dialects::ipc::{FeatureBucket, GeometryPath, PathCmd, PathOp};

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
        let artwork = artwork_from_processed_layer(ipc, &doc, 0, plan.file_function.clone())?;
        let layer = lower_artwork_layer(&artwork)?;
        let contents = write_layer(&layer)?;
        files.push(GerberExportFile {
            filename: plan.filename.clone(),
            layer,
            contents,
        });
    }

    if let Some(doc) = first_doc {
        let profile = profile_artwork_from_outlines(&doc)?;
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
    file_function: Vec<String>,
) -> Result<ArtworkLayer> {
    let layer = &doc.layers[layer_index];
    let mut objects = Vec::new();
    for feature in &doc.features
        [layer.feature_start as usize..(layer.feature_start + layer.feature_count) as usize]
    {
        for path in &doc.paths
            [feature.path_start as usize..(feature.path_start + feature.path_count) as usize]
        {
            if path.flags.filled {
                objects.push(ArtworkObject::Region {
                    contours: artwork_contours(doc, path)?,
                    attributes: object_attributes(ipc, feature.net),
                });
            } else if path.flags.stroked {
                objects.push(ArtworkObject::Stroke {
                    width: path.stroke_width,
                    contours: artwork_contours(doc, path)?,
                    aperture_function: aperture_function(feature.bucket).to_string(),
                    attributes: object_attributes(ipc, feature.net),
                });
            } else {
                bail!(
                    "processed IPC geometry path is neither filled nor stroked on layer '{}'",
                    layer.name
                );
            }
        }
    }
    Ok(ArtworkLayer {
        file_function,
        objects,
    })
}

fn profile_artwork_from_outlines(
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
) -> Result<ArtworkLayer> {
    let mut objects = Vec::new();
    for outline in &doc.board_outlines {
        for path in &doc.paths
            [outline.path_start as usize..(outline.path_start + outline.path_count) as usize]
        {
            if path.flags.stroked {
                objects.push(ArtworkObject::Stroke {
                    width: path.stroke_width,
                    contours: artwork_contours(doc, path)?,
                    aperture_function: "Profile".to_string(),
                    attributes: ObjectAttributes::default(),
                });
            } else if path.flags.filled {
                objects.push(ArtworkObject::Region {
                    contours: artwork_contours(doc, path)?,
                    attributes: ObjectAttributes::default(),
                });
            } else {
                bail!("processed IPC board outline path is neither filled nor stroked");
            }
        }
    }
    Ok(ArtworkLayer {
        file_function: vec!["Profile".into(), "NP".into()],
        objects,
    })
}

fn object_attributes(ipc: &Ipc2581, net: Option<ipc2581::Symbol>) -> ObjectAttributes {
    ObjectAttributes {
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

fn artwork_contours(
    doc: &pcb_ir::dialects::ipc::GeometryDocument<ipc2581::Symbol, LayerFunction>,
    path: &GeometryPath,
) -> Result<Vec<ArtworkContour>> {
    let mut contours = Vec::new();
    for contour in &doc.contours
        [path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
    {
        let cmds = &doc.path_cmds
            [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize];
        contours.push(ArtworkContour {
            segments: artwork_segments(cmds)?,
        });
    }
    Ok(contours)
}

fn artwork_segments(cmds: &[PathCmd]) -> Result<Vec<ArtworkSegment>> {
    let mut first = None;
    let mut current = None;
    let mut segments = Vec::new();
    for cmd in cmds {
        match cmd.op {
            PathOp::MoveTo => {
                first = Some(cmd.p0);
                current = Some(cmd.p0);
            }
            PathOp::LineTo => {
                let start = current.context("path line command appears before move command")?;
                segments.push(ArtworkSegment::Line { start, end: cmd.p0 });
                current = Some(cmd.p0);
            }
            PathOp::ArcTo => {
                let start = current.context("path arc command appears before move command")?;
                segments.push(ArtworkSegment::Arc {
                    start,
                    end: cmd.p0,
                    center: cmd.p1,
                    clockwise: cmd.clockwise,
                });
                current = Some(cmd.p0);
            }
            PathOp::CubicTo => {
                let start = current.context("path cubic command appears before move command")?;
                let steps = 16;
                for step in 1..=steps {
                    let end =
                        cubic_point(start, cmd.p0, cmd.p1, cmd.p2, step as f64 / steps as f64);
                    let segment_start = current.unwrap_or(start);
                    segments.push(ArtworkSegment::Line {
                        start: segment_start,
                        end,
                    });
                    current = Some(end);
                }
            }
            PathOp::Close => {
                if let (Some(start), Some(end)) = (first, current)
                    && !points_close(start, end)
                {
                    segments.push(ArtworkSegment::Line {
                        start: end,
                        end: start,
                    });
                }
                current = first;
            }
        }
    }
    Ok(segments)
}

fn points_close(a: Point, b: Point) -> bool {
    a.distance_to(b) <= 1e-9
}

fn cubic_point(start: Point, c1: Point, c2: Point, end: Point, t: f64) -> Point {
    let mt = 1.0 - t;
    Point::new(
        mt.powi(3) * start.x
            + 3.0 * mt.powi(2) * t * c1.x
            + 3.0 * mt * t.powi(2) * c2.x
            + t.powi(3) * end.x,
        mt.powi(3) * start.y
            + 3.0 * mt.powi(2) * t * c1.y
            + 3.0 * mt * t.powi(2) * c2.y
            + t.powi(3) * end.y,
    )
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
}
