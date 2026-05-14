use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gerberx2::{GerberLayer, write_layer};
use ipc2581::Ipc2581;
use ipc2581::types::{LayerFunction, Side};

use super::artwork::{ArtworkContour, ArtworkLayer, ArtworkObject, ArtworkSegment};
use super::lower::lower_artwork_layer;
use crate::geometry::ir::{GeometryDocument, GeometryPath, PathCmd, PathOp, Point};
use crate::{geometry, ipc2581 as ipc};

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

    for source_layer in &ecad.cad_data.layers {
        if !is_gerber_layer_function(source_layer.layer_function) {
            continue;
        }
        let layer_name = ipc.resolve(source_layer.name);
        let mut doc = geometry::extract_layer(ipc, layer_name)
            .with_context(|| format!("failed to extract IPC-2581 layer '{layer_name}'"))?;
        geometry::process::process_document(&mut doc);
        if first_doc.is_none() {
            first_doc = Some(doc.clone());
        }
        let artwork = artwork_from_processed_layer(&doc, 0, source_layer.side)?;
        let layer = lower_artwork_layer(&artwork)?;
        let contents = write_layer(&layer)?;
        files.push(GerberExportFile {
            filename: format!("{}.gbr", sanitize_filename(layer_name)),
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

fn is_gerber_layer_function(function: LayerFunction) -> bool {
    matches!(
        function,
        LayerFunction::Conductor
            | LayerFunction::CondFilm
            | LayerFunction::CondFoil
            | LayerFunction::Plane
            | LayerFunction::Signal
            | LayerFunction::Mixed
            | LayerFunction::Solderpaste
            | LayerFunction::Pastemask
            | LayerFunction::Soldermask
            | LayerFunction::Silkscreen
            | LayerFunction::Legend
            | LayerFunction::Rout
            | LayerFunction::BoardOutline
    )
}

fn artwork_from_processed_layer(
    doc: &GeometryDocument,
    layer_index: usize,
    side: Option<Side>,
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
                });
            } else if path.flags.stroked {
                objects.push(ArtworkObject::Stroke {
                    width: path.stroke_width,
                    line_cap: path.line_cap,
                    contours: artwork_contours(doc, path)?,
                });
            } else {
                bail!(
                    "IPC geometry path is neither filled nor stroked on layer '{}'",
                    layer.name
                );
            }
        }
    }
    Ok(ArtworkLayer {
        function: layer.layer_function,
        side,
        objects,
    })
}

fn profile_artwork_from_outlines(doc: &GeometryDocument) -> Result<ArtworkLayer> {
    let mut objects = Vec::new();
    for outline in &doc.board_outlines {
        for path in &doc.paths
            [outline.path_start as usize..(outline.path_start + outline.path_count) as usize]
        {
            if path.flags.stroked {
                objects.push(ArtworkObject::Stroke {
                    width: path.stroke_width,
                    line_cap: path.line_cap,
                    contours: artwork_contours(doc, path)?,
                });
            } else if path.flags.filled {
                objects.push(ArtworkObject::Region {
                    contours: artwork_contours(doc, path)?,
                });
            } else {
                bail!("IPC board outline path is neither filled nor stroked");
            }
        }
    }
    Ok(ArtworkLayer {
        function: LayerFunction::BoardOutline,
        side: None,
        objects,
    })
}

fn artwork_contours(doc: &GeometryDocument, path: &GeometryPath) -> Result<Vec<ArtworkContour>> {
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

fn sanitize_filename(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "layer".to_string()
    } else {
        out
    }
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
          <Set>
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

        assert!(set.files.iter().any(|file| file.filename == "TOP.gbr"));
        assert!(set.files.iter().any(|file| file.filename == "profile.gbr"));
        for file in &set.files {
            gerberx2::GerberX2::parse(&file.contents).unwrap();
        }
    }
}
