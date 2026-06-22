use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use gerberx2::{
    AttributeValue, Contour, ContourSegment, GerberLayer, ObjectKind, Point as GerberPoint,
    WriterAperture, WriterApertureTemplate, WriterObject, sanitize_attribute_field,
};
use pcb_ir::common::{PaintPolarity, Point};
use pcb_ir::dialects::artwork::{ArtworkGeometry, ArtworkPath};
use pcb_ir::dialects::gerber::Polarity;
use pcb_ir::dialects::path::{PathCmd, PathOp};

use super::artwork::{ArtworkLayer, LayerAttributes, ObjectAttributes};

pub fn lower_artwork_layer(layer: &ArtworkLayer) -> Result<GerberLayer> {
    let mut apertures = ApertureTable::default();
    let mut objects = Vec::new();
    let layer_attributes = layer
        .layers
        .first()
        .map(|layer| layer.meta.clone())
        .unwrap_or_default();

    for object in &layer.objects {
        let attributes = lower_object_attributes(&object.meta);
        match object.geometry {
            ArtworkGeometry::Region { path } => objects.push(WriterObject {
                kind: ObjectKind::Region {
                    contours: lower_region_contours(layer, path)?,
                },
                polarity: lower_polarity(object.paint),
                attributes,
            }),
            ArtworkGeometry::Stroke { path } => {
                let artwork_path = &layer.paths[path as usize];
                let default_function = vec!["Conductor".to_string()];
                let aperture_function = object
                    .meta
                    .aperture_function
                    .as_deref()
                    .unwrap_or(default_function.as_slice());
                let aperture = apertures.circle(artwork_path.stroke_width, aperture_function)?;
                for contour in path_contours(layer, artwork_path) {
                    for segment in contour_segments(&contour.cmds)? {
                        objects.push(WriterObject {
                            kind: match segment {
                                Segment::Line { start, end } => ObjectKind::Draw {
                                    start: lower_point(start),
                                    end: lower_point(end),
                                    aperture,
                                },
                                Segment::Arc {
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
                            polarity: lower_polarity(object.paint),
                            attributes: attributes.clone(),
                        });
                    }
                }
            }
            ArtworkGeometry::Flash { .. } => {
                bail!("cannot lower unexpanded artwork flash to Gerber")
            }
        }
    }

    Ok(GerberLayer {
        file_attributes: lower_layer_attributes(&layer_attributes),
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
    function: Vec<String>,
}

impl ApertureTable {
    fn circle(&mut self, diameter: f64, function: &[String]) -> Result<i32> {
        if diameter <= 0.0 {
            bail!("cannot export non-positive Gerber stroke aperture diameter {diameter}");
        }
        let key = ApertureKey {
            diameter_nm: quantize_mm(diameter),
            function: function.to_vec(),
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
            attributes: vec![AttributeValue::new(
                ".AperFunction",
                function.iter().cloned(),
            )],
        });
        Ok(code)
    }

    fn into_apertures(self) -> Vec<WriterAperture> {
        self.apertures
    }
}

fn lower_layer_attributes(attributes: &LayerAttributes) -> Vec<AttributeValue> {
    let mut values = vec![AttributeValue::new(
        ".FileFunction",
        attributes.file_function.iter().cloned(),
    )];
    if let Some(part) = &attributes.part {
        values.push(AttributeValue::new(".Part", part.iter().cloned()));
    }
    if let Some(file_polarity) = &attributes.file_polarity {
        values.push(AttributeValue::new(
            ".FilePolarity",
            [file_polarity.clone()],
        ));
    }
    values
}

fn lower_region_contours(layer: &ArtworkLayer, path: u32) -> Result<Vec<Contour>> {
    path_contours(layer, &layer.paths[path as usize])
        .into_iter()
        .map(|contour| {
            if contour.cmds.is_empty() {
                bail!("cannot export empty Gerber region contour");
            }
            Ok(Contour {
                segments: contour_segments(&contour.cmds)?
                    .into_iter()
                    .map(|segment| match segment {
                        Segment::Line { start, end } => ContourSegment::Line {
                            start: lower_point(start),
                            end: lower_point(end),
                        },
                        Segment::Arc {
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

fn path_contours(
    layer: &ArtworkLayer,
    path: &ArtworkPath,
) -> Vec<pcb_ir::dialects::path::PathPayload> {
    layer.contours[path.contour_start as usize..(path.contour_start + path.contour_count) as usize]
        .iter()
        .map(|contour| pcb_ir::dialects::path::PathPayload {
            bbox: contour.bbox,
            cmds: layer.path_cmds
                [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
                .to_vec(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum Segment {
    Line {
        start: Point,
        end: Point,
    },
    Arc {
        start: Point,
        end: Point,
        center: Point,
        clockwise: bool,
    },
}

fn contour_segments(cmds: &[PathCmd]) -> Result<Vec<Segment>> {
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
                segments.push(Segment::Line { start, end: cmd.p0 });
                current = Some(cmd.p0);
            }
            PathOp::ArcTo => {
                let start = current.context("path arc command appears before move command")?;
                segments.push(Segment::Arc {
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
                    segments.push(Segment::Line {
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
                    segments.push(Segment::Line {
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

fn lower_object_attributes(attributes: &ObjectAttributes) -> Vec<AttributeValue> {
    let mut values = Vec::new();
    if let Some(component) = &attributes.component {
        values.push(AttributeValue::new(
            ".C",
            [sanitize_attribute_field(component)],
        ));
    }
    if let (Some(component), Some(pin)) = (&attributes.component, &attributes.pin) {
        values.push(AttributeValue::new(
            ".P",
            [
                sanitize_attribute_field(component),
                sanitize_attribute_field(pin),
            ],
        ));
    }
    if let Some(net) = &attributes.net {
        values.push(AttributeValue::new(".N", [sanitize_attribute_field(net)]));
    }
    values
}

fn lower_polarity(paint: PaintPolarity) -> Polarity {
    match paint {
        PaintPolarity::Dark => Polarity::Dark,
        PaintPolarity::Clear => Polarity::Clear,
    }
}

fn lower_point(point: Point) -> GerberPoint {
    GerberPoint {
        x: point.x,
        y: point.y,
    }
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

fn quantize_mm(value: f64) -> i64 {
    (value * 1_000_000.0).round() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_net_names_for_gerber_attribute_fields() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            aperture_function: None,
            net: Some("PWR_RST*,A%B".to_string()),
            component: None,
            pin: None,
        });

        assert_eq!(attributes[0].name, ".N");
        assert_eq!(attributes[0].fields, ["PWR_RST__A_B"]);
    }

    #[test]
    fn lowers_pin_attribute_with_component_context() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            aperture_function: None,
            net: None,
            component: Some("U1".to_string()),
            pin: Some("1".to_string()),
        });

        assert_eq!(attributes[0].name, ".C");
        assert_eq!(attributes[0].fields, ["U1"]);
        assert_eq!(attributes[1].name, ".P");
        assert_eq!(attributes[1].fields, ["U1", "1"]);
    }

    #[test]
    fn skips_pin_attribute_without_component_context() {
        let attributes = lower_object_attributes(&ObjectAttributes {
            aperture_function: None,
            net: None,
            component: None,
            pin: Some("1".to_string()),
        });

        assert!(attributes.is_empty());
    }
}
