use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use gerberx2::{
    AttributeValue, Contour, ContourSegment, GerberLayer, ObjectKind, Point as GerberPoint,
    WriterAperture, WriterApertureTemplate, WriterObject, sanitize_attribute_field,
};
use pcb_ir::dialects::gerber::Polarity;

use super::artwork::{
    ArtworkContour, ArtworkLayer, ArtworkObject, ArtworkSegment, ObjectAttributes,
};
use pcb_ir::common::Point;

pub fn lower_artwork_layer(layer: &ArtworkLayer) -> Result<GerberLayer> {
    let mut apertures = ApertureTable::default();
    let mut objects = Vec::new();

    for object in &layer.objects {
        match object {
            ArtworkObject::Region {
                contours,
                attributes,
            } => objects.push(WriterObject {
                kind: ObjectKind::Region {
                    contours: lower_region_contours(contours)?,
                },
                polarity: Polarity::Dark,
                attributes: lower_object_attributes(attributes),
            }),
            ArtworkObject::Stroke {
                width,
                contours,
                aperture_function,
                attributes,
            } => {
                let aperture = apertures.circle(*width, aperture_function)?;
                for contour in contours {
                    for segment in &contour.segments {
                        objects.push(WriterObject {
                            kind: match *segment {
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
                            },
                            polarity: Polarity::Dark,
                            attributes: lower_object_attributes(attributes),
                        });
                    }
                }
            }
        }
    }

    Ok(GerberLayer {
        file_attributes: vec![AttributeValue::new(
            ".FileFunction",
            layer.file_function.iter().cloned(),
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ApertureKey {
    diameter_nm: i64,
    function: String,
}

impl ApertureTable {
    fn circle(&mut self, diameter: f64, function: &str) -> Result<i32> {
        if diameter <= 0.0 {
            bail!("cannot export non-positive Gerber stroke aperture diameter {diameter}");
        }
        let key = ApertureKey {
            diameter_nm: quantize_mm(diameter),
            function: function.to_string(),
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
            attributes: vec![AttributeValue::new(".AperFunction", [function.to_string()])],
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

fn lower_object_attributes(attributes: &ObjectAttributes) -> Vec<AttributeValue> {
    attributes
        .net
        .as_ref()
        .map(|net| vec![AttributeValue::new(".N", [sanitize_attribute_field(net)])])
        .unwrap_or_default()
}

fn lower_point(point: Point) -> GerberPoint {
    GerberPoint {
        x: point.x,
        y: point.y,
    }
}

fn quantize_mm(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_net_names_for_gerber_attribute_fields() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            net: Some("PWR_RST*,A%B".to_string()),
        });

        assert_eq!(attributes[0].name, ".N");
        assert_eq!(attributes[0].fields, ["PWR_RST__A_B"]);
    }
}
