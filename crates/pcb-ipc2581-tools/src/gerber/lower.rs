use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use gerberx2::{
    AttributeValue, Contour, ContourSegment, GerberLayer, ObjectKind, Point as GerberPoint,
    WriterAperture, WriterApertureTemplate, WriterObject,
};
use ipc2581::types::{LayerFunction, Side};

use super::artwork::{ArtworkContour, ArtworkLayer, ArtworkObject, ArtworkSegment};
use crate::geometry::ir::{LineCap, Point};

pub fn lower_artwork_layer(layer: &ArtworkLayer) -> Result<GerberLayer> {
    let mut apertures = ApertureTable::default();
    let mut objects = Vec::new();

    for object in &layer.objects {
        match object {
            ArtworkObject::Region { contours } => {
                objects.push(WriterObject::dark(ObjectKind::Region {
                    contours: lower_region_contours(contours)?,
                }));
            }
            ArtworkObject::Stroke {
                width,
                line_cap,
                contours,
            } => {
                let aperture = apertures.circle(*width, *line_cap)?;
                for contour in contours {
                    for segment in &contour.segments {
                        objects.push(WriterObject::dark(match *segment {
                            ArtworkSegment::Line { start, end } => ObjectKind::Draw {
                                start: lower_point(start),
                                end: lower_point(end),
                                aperture,
                            },
                            ArtworkSegment::Arc {
                                start,
                                end,
                                center,
                                clockwise,
                            } => ObjectKind::Arc {
                                start: lower_point(start),
                                end: lower_point(end),
                                center_offset: lower_point(Point::new(
                                    center.x - start.x,
                                    center.y - start.y,
                                )),
                                clockwise,
                                aperture,
                            },
                        }));
                    }
                }
            }
        }
    }

    Ok(GerberLayer {
        file_attributes: vec![AttributeValue::new(
            ".FileFunction",
            file_function_fields(layer.function, layer.side),
        )],
        apertures: apertures.into_apertures(),
        objects,
        ..GerberLayer::default()
    })
}

#[derive(Default)]
struct ApertureTable {
    next_code: i32,
    by_key: HashMap<ApertureKey, i32>,
    apertures: Vec<WriterAperture>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ApertureKey {
    diameter_nm: i64,
    line_cap: LineCap,
}

impl ApertureTable {
    fn circle(&mut self, diameter: f64, line_cap: LineCap) -> Result<i32> {
        if diameter <= 0.0 {
            bail!("cannot export non-positive Gerber stroke aperture diameter {diameter}");
        }
        let key = ApertureKey {
            diameter_nm: quantize_mm(diameter),
            line_cap,
        };
        if let Some(code) = self.by_key.get(&key) {
            return Ok(*code);
        }
        let code = if self.next_code == 0 {
            self.next_code = 10;
            10
        } else {
            self.next_code += 1;
            self.next_code
        };
        self.by_key.insert(key, code);
        self.apertures.push(WriterAperture {
            code,
            template: WriterApertureTemplate::Circle {
                diameter,
                hole_diameter: None,
            },
            attributes: Vec::new(),
        });
        Ok(code)
    }

    fn into_apertures(self) -> Vec<WriterAperture> {
        self.apertures
    }
}

fn lower_region_contours(contours: &[ArtworkContour]) -> Result<Vec<Contour>> {
    contours
        .iter()
        .map(|contour| {
            if contour.segments.is_empty() {
                bail!("cannot export empty Gerber region contour");
            }
            Ok(Contour {
                segments: contour
                    .segments
                    .iter()
                    .map(|segment| match *segment {
                        ArtworkSegment::Line { start, end } => ContourSegment::Line {
                            start: lower_point(start),
                            end: lower_point(end),
                        },
                        ArtworkSegment::Arc {
                            start,
                            end,
                            center,
                            clockwise,
                        } => ContourSegment::Arc {
                            start: lower_point(start),
                            end: lower_point(end),
                            center_offset: lower_point(Point::new(
                                center.x - start.x,
                                center.y - start.y,
                            )),
                            clockwise,
                        },
                    })
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>>>()
        .context("failed to lower artwork contours to Gerber regions")
}

fn lower_point(point: Point) -> GerberPoint {
    GerberPoint {
        x: point.x,
        y: point.y,
    }
}

fn file_function_fields(function: LayerFunction, side: Option<Side>) -> Vec<String> {
    match function {
        LayerFunction::Conductor
        | LayerFunction::CondFilm
        | LayerFunction::CondFoil
        | LayerFunction::Plane
        | LayerFunction::Signal
        | LayerFunction::Mixed => vec![
            "Copper".to_string(),
            side_layer_field(side).to_string(),
            side_field(side).to_string(),
        ],
        LayerFunction::Solderpaste | LayerFunction::Pastemask => {
            vec!["Paste".to_string(), side_field(side).to_string()]
        }
        LayerFunction::Soldermask => vec!["Soldermask".to_string(), side_field(side).to_string()],
        LayerFunction::Silkscreen | LayerFunction::Legend => {
            vec!["Legend".to_string(), side_field(side).to_string()]
        }
        LayerFunction::Rout | LayerFunction::BoardOutline => {
            vec!["Profile".to_string(), "NP".to_string()]
        }
        LayerFunction::Drill => vec!["Drill".to_string(), "PTH".to_string()],
        _ => vec![format!("{:?}", function)],
    }
}

fn side_field(side: Option<Side>) -> &'static str {
    match side {
        Some(Side::Top) => "Top",
        Some(Side::Bottom) => "Bot",
        Some(Side::Internal) => "Inr",
        _ => "Other",
    }
}

fn side_layer_field(side: Option<Side>) -> &'static str {
    match side {
        Some(Side::Top) => "L1",
        Some(Side::Bottom) => "L2",
        _ => "L0",
    }
}

fn quantize_mm(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}
