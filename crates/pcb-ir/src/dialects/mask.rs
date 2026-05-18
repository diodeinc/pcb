use crate::common::*;
use crate::dialects::path::{self, PathCmd, PathPayload};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use resvg::{tiny_skia, usvg};
use std::fmt::Write as FmtWrite;
use std::io::{self, IsTerminal, Write as IoWrite};
use terminal_size::{Width, terminal_size};

const VIEWBOX_PADDING_MM: f64 = 1.0;
const DEFAULT_MAX_DIMENSION_PX: u32 = 3200;
const KITTY_CHUNK_SIZE: usize = 4096;
const MAX_TERMINAL_DIMENSION_PX: u32 = 1200;
const POINT_EPSILON_MM: f64 = 1e-9;

/// Fully composed layer image geometry.
///
/// The mask dialect is the common render/compare target after ordered paint
/// operations have been resolved. It intentionally stores only final positive
/// shapes per layer; dark/clear ordering belongs in artwork/geom lowering.
#[derive(Debug, Clone)]
pub struct MaskDocument<LayerMeta = ()> {
    pub unit: Unit,
    pub layers: Vec<MaskLayer<LayerMeta>>,
    pub shapes: Vec<MaskShape>,
    pub contours: Vec<MaskContour>,
    pub path_cmds: Vec<PathCmd>,
    pub diagnostics: Vec<GeometryDiagnostic>,
}

impl<LayerMeta> MaskDocument<LayerMeta> {
    pub fn new(unit: Unit) -> Self {
        Self {
            unit,
            layers: Vec::new(),
            shapes: Vec::new(),
            contours: Vec::new(),
            path_cmds: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    pub fn push_layer(&mut self, mut layer: MaskLayer<LayerMeta>) -> u32 {
        layer.shape_start = self.shapes.len() as u32;
        layer.shape_count = 0;
        let id = self.layers.len() as u32;
        self.layers.push(layer);
        id
    }

    pub fn push_shape(&mut self, layer_id: u32, mut shape: MaskShape, contours: Vec<PathPayload>) {
        let contour_start = self.contours.len() as u32;
        let mut bbox = BBox::empty();
        for contour in contours {
            bbox = bbox.union(contour.bbox);
            self.push_contour(contour);
        }
        shape.contour_start = contour_start;
        shape.contour_count = self.contours.len() as u32 - contour_start;
        shape.bbox = bbox;

        self.shapes.push(shape);
        let layer = &mut self.layers[layer_id as usize];
        if layer.shape_count == 0 {
            layer.shape_start = self.shapes.len() as u32 - 1;
        }
        layer.shape_count += 1;
        layer.bbox = layer.bbox.union(bbox);
    }

    fn push_contour(&mut self, contour: PathPayload) {
        let cmd_start = self.path_cmds.len() as u32;
        self.path_cmds.extend(contour.cmds);
        self.contours.push(MaskContour {
            cmd_start,
            cmd_count: self.path_cmds.len() as u32 - cmd_start,
            bbox: contour.bbox,
        });
    }

    pub fn validate(&self) -> Result<(), String> {
        for (index, layer) in self.layers.iter().enumerate() {
            validate_range(
                "mask layer shapes",
                index,
                layer.shape_start,
                layer.shape_count,
                self.shapes.len(),
            )?;
            validate_bbox("mask layer", index, layer.bbox)?;
        }
        for (index, shape) in self.shapes.iter().enumerate() {
            validate_range(
                "mask shape contours",
                index,
                shape.contour_start,
                shape.contour_count,
                self.contours.len(),
            )?;
            validate_bbox("mask shape", index, shape.bbox)?;
        }
        for (index, contour) in self.contours.iter().enumerate() {
            validate_range(
                "mask contour commands",
                index,
                contour.cmd_start,
                contour.cmd_count,
                self.path_cmds.len(),
            )?;
            validate_bbox("mask contour", index, contour.bbox)?;
        }
        path::validate_cmd_points("mask", &self.path_cmds)
    }
}

pub fn render_svg<LayerMeta>(doc: &MaskDocument<LayerMeta>, layer_index: usize) -> String {
    render_svg_layers_with_size(doc, &[layer_index], None)
}

pub fn render_svg_sized<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_index: usize,
    width_px: u32,
    height_px: u32,
) -> String {
    render_svg_layers_with_size(doc, &[layer_index], Some((width_px, height_px)))
}

pub fn render_svg_layers<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
) -> String {
    render_svg_layers_with_size(doc, layer_indices, None)
}

pub fn render_svg_layers_sized<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
    width_px: u32,
    height_px: u32,
) -> String {
    render_svg_layers_with_size(doc, layer_indices, Some((width_px, height_px)))
}

fn render_svg_layers_with_size<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
    pixel_size: Option<(u32, u32)>,
) -> String {
    let bbox = render_layers_bbox(doc, layer_indices);
    let viewbox_y = -bbox.max.y;
    let mut svg = String::new();
    let size = pixel_size
        .map(|(width, height)| format!(" width='{width}' height='{height}'"))
        .unwrap_or_default();
    writeln!(
        svg,
        "<svg xmlns='http://www.w3.org/2000/svg'{size} viewBox='{} {} {} {}'>",
        fmt_num(bbox.min.x),
        fmt_num(viewbox_y),
        fmt_num(bbox.width()),
        fmt_num(bbox.height())
    )
    .unwrap();
    let title = layer_indices
        .first()
        .and_then(|&index| doc.layers.get(index))
        .map(|layer| layer.name.as_str())
        .unwrap_or("mask");
    writeln!(svg, "  <title>{}</title>", escape_xml(title)).unwrap();
    writeln!(svg, "  <g transform='scale(1 -1)'>").unwrap();

    for &layer_index in layer_indices {
        let layer = &doc.layers[layer_index];
        for shape in &doc.shapes
            [layer.shape_start as usize..(layer.shape_start + layer.shape_count) as usize]
        {
            write_shape(&mut svg, doc, layer, shape);
        }
    }

    writeln!(svg, "  </g>").unwrap();
    writeln!(svg, "</svg>").unwrap();
    svg
}

pub fn render_png<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_index: usize,
) -> Result<Vec<u8>, String> {
    render_png_with_max_dimension(doc, layer_index, DEFAULT_MAX_DIMENSION_PX)
}

pub fn render_png_with_max_dimension<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_index: usize,
    max_dimension_px: u32,
) -> Result<Vec<u8>, String> {
    let (width_px, height_px) = pixel_size(doc, &[layer_index], max_dimension_px);
    let svg = render_svg_sized(doc, layer_index, width_px, height_px);
    svg_to_png(&svg)
}

pub fn render_png_layers<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
) -> Result<Vec<u8>, String> {
    render_png_layers_with_max_dimension(doc, layer_indices, DEFAULT_MAX_DIMENSION_PX)
}

pub fn render_png_layers_with_max_dimension<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
    max_dimension_px: u32,
) -> Result<Vec<u8>, String> {
    let (width_px, height_px) = pixel_size(doc, layer_indices, max_dimension_px);
    let svg = render_svg_layers_sized(doc, layer_indices, width_px, height_px);
    svg_to_png(&svg)
}

pub fn can_render_to_terminal() -> bool {
    io::stdout().is_terminal()
}

pub fn render_to_terminal<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_index: usize,
) -> Result<(), String> {
    if !io::stdout().is_terminal() {
        return Err(
            "stdout is not an interactive terminal; pass an SVG or PNG output path".to_string(),
        );
    }
    let png = render_png_with_max_dimension(doc, layer_index, terminal_max_dimension_px())?;
    let mut stdout = io::stdout().lock();
    write_kitty_png(&mut stdout, &png).map_err(|err| err.to_string())?;
    stdout.write_all(b"\n").map_err(|err| err.to_string())?;
    Ok(())
}

pub fn render_layers_to_terminal<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
) -> Result<(), String> {
    if !io::stdout().is_terminal() {
        return Err(
            "stdout is not an interactive terminal; pass an SVG or PNG output path".to_string(),
        );
    }
    let png =
        render_png_layers_with_max_dimension(doc, layer_indices, terminal_max_dimension_px())?;
    let mut stdout = io::stdout().lock();
    write_kitty_png(&mut stdout, &png).map_err(|err| err.to_string())?;
    stdout.write_all(b"\n").map_err(|err| err.to_string())?;
    Ok(())
}

pub fn render_bbox<LayerMeta>(doc: &MaskDocument<LayerMeta>, layer_index: usize) -> BBox {
    render_layers_bbox(doc, &[layer_index])
}

pub fn render_layers_bbox<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
) -> BBox {
    let bbox = layer_indices.iter().fold(BBox::empty(), |bbox, &index| {
        bbox.union(doc.layers[index].bbox)
    });
    if bbox.is_empty() {
        BBox {
            min: Point::new(0.0, 0.0),
            max: Point::new(100.0, 100.0),
        }
    } else {
        bbox.expand(VIEWBOX_PADDING_MM)
    }
}

fn write_shape<LayerMeta>(
    svg: &mut String,
    doc: &MaskDocument<LayerMeta>,
    layer: &MaskLayer<LayerMeta>,
    shape: &MaskShape,
) {
    let d = path_data(doc, shape);
    if d.is_empty() {
        return;
    }
    let fill_rule = match shape.fill_rule {
        FillRule::NonZero => "nonzero",
        FillRule::EvenOdd => "evenodd",
    };
    let (color, opacity) = layer_style(layer.role);
    if layer.role == LayerRole::Profile {
        writeln!(
            svg,
            "    <path d='{d}' fill='none' stroke='#000000' stroke-width='0.1' stroke-linejoin='round' data-board-outline='true'/>",
        )
        .unwrap();
    } else {
        writeln!(
            svg,
            "    <path d='{d}' fill='{color}' fill-opacity='{}' fill-rule='{fill_rule}'/>",
            fmt_num(opacity)
        )
        .unwrap();
    }
}

fn path_data<LayerMeta>(doc: &MaskDocument<LayerMeta>, shape: &MaskShape) -> String {
    let mut data = String::new();
    for contour in &doc.contours
        [shape.contour_start as usize..(shape.contour_start + shape.contour_count) as usize]
    {
        let mut current = Point::default();
        for cmd in &doc.path_cmds
            [contour.cmd_start as usize..(contour.cmd_start + contour.cmd_count) as usize]
        {
            match cmd.op {
                crate::dialects::path::PathOp::MoveTo => {
                    current = cmd.p0;
                    if !data.is_empty() {
                        data.push(' ');
                    }
                    write!(data, "M{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
                }
                crate::dialects::path::PathOp::LineTo => {
                    current = cmd.p0;
                    write!(data, " L{} {}", fmt_num(cmd.p0.x), fmt_num(cmd.p0.y)).unwrap();
                }
                crate::dialects::path::PathOp::ArcTo => {
                    write_arc_to_path_data(&mut data, current, cmd.p0, cmd.p1, cmd.clockwise);
                    current = cmd.p0;
                }
                crate::dialects::path::PathOp::CubicTo => {
                    current = cmd.p2;
                    write!(
                        data,
                        " C{} {},{} {},{} {}",
                        fmt_num(cmd.p0.x),
                        fmt_num(cmd.p0.y),
                        fmt_num(cmd.p1.x),
                        fmt_num(cmd.p1.y),
                        fmt_num(cmd.p2.x),
                        fmt_num(cmd.p2.y)
                    )
                    .unwrap();
                }
                crate::dialects::path::PathOp::Close => data.push_str(" Z"),
            }
        }
    }
    data
}

fn write_arc_to_path_data(
    data: &mut String,
    start: Point,
    end: Point,
    center: Point,
    clockwise: bool,
) {
    let radius = start.distance_to(center);
    if radius <= POINT_EPSILON_MM {
        write!(data, " L{} {}", fmt_num(end.x), fmt_num(end.y)).unwrap();
        return;
    }

    let sweep_flag = if clockwise { 0 } else { 1 };
    if start.distance_to(end) <= POINT_EPSILON_MM {
        let midpoint = Point::new(2.0 * center.x - start.x, 2.0 * center.y - start.y);
        write_svg_arc(data, radius, 0, sweep_flag, midpoint);
        write_svg_arc(data, radius, 0, sweep_flag, end);
        return;
    }

    let large_arc =
        u8::from(arc_sweep_radians(start, end, center, clockwise) > std::f64::consts::PI);
    write_svg_arc(data, radius, large_arc, sweep_flag, end);
}

fn write_svg_arc(data: &mut String, radius: f64, large_arc: u8, sweep_flag: u8, end: Point) {
    write!(
        data,
        " A{} {} 0 {large_arc} {sweep_flag} {} {}",
        fmt_num(radius),
        fmt_num(radius),
        fmt_num(end.x),
        fmt_num(end.y)
    )
    .unwrap();
}

fn layer_style(role: LayerRole) -> (&'static str, f64) {
    match role {
        LayerRole::Copper => ("#d87822", 0.9),
        LayerRole::Soldermask => ("#159447", 0.55),
        LayerRole::Paste => ("#aeb4bb", 0.9),
        LayerRole::Legend => ("#f8f9fa", 0.95),
        LayerRole::Profile => ("#000000", 1.0),
        LayerRole::Drill | LayerRole::Mechanical | LayerRole::Other => ("#5c7cfa", 0.85),
    }
}

fn pixel_size<LayerMeta>(
    doc: &MaskDocument<LayerMeta>,
    layer_indices: &[usize],
    max_dimension_px: u32,
) -> (u32, u32) {
    let bbox = render_layers_bbox(doc, layer_indices);
    if bbox.is_empty() || bbox.width() <= 0.0 || bbox.height() <= 0.0 {
        return (max_dimension_px, max_dimension_px);
    }
    let scale = max_dimension_px as f64 / bbox.width().max(bbox.height());
    (
        (bbox.width() * scale).ceil().max(1.0) as u32,
        (bbox.height() * scale).ceil().max(1.0) as u32,
    )
}

fn svg_to_png(svg: &str) -> Result<Vec<u8>, String> {
    let options = usvg::Options::default();
    let tree = usvg::Tree::from_data(svg.as_bytes(), &options)
        .map_err(|err| format!("failed to parse SVG: {err}"))?;
    let size = tree.size();
    let width = size.width().ceil().max(1.0) as u32;
    let height = size.height().ceil().max(1.0) as u32;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| format!("failed to allocate {width}x{height} PNG raster"))?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    pixmap
        .encode_png()
        .map_err(|err| format!("failed to encode PNG: {err}"))
}

fn terminal_max_dimension_px() -> u32 {
    let Some((Width(columns), _)) = terminal_size() else {
        return MAX_TERMINAL_DIMENSION_PX;
    };
    u32::from(columns)
        .saturating_mul(12)
        .clamp(1, MAX_TERMINAL_DIMENSION_PX)
}

pub fn write_kitty_png<W: IoWrite>(writer: &mut W, png: &[u8]) -> io::Result<()> {
    let encoded = STANDARD.encode(png);
    let mut chunks = encoded.as_bytes().chunks(KITTY_CHUNK_SIZE).peekable();
    let mut first = true;
    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        if first {
            write!(writer, "\x1b_Ga=T,f=100,m={more};")?;
            first = false;
        } else {
            write!(writer, "\x1b_Gm={more};")?;
        }
        writer.write_all(chunk)?;
        writer.write_all(b"\x1b\\")?;
    }
    Ok(())
}

fn escape_xml(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn fmt_num(value: f64) -> String {
    let mut text = format!("{value:.6}");
    while text.contains('.') && text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    if text == "-0" { "0".to_string() } else { text }
}

#[derive(Debug, Clone)]
pub struct MaskLayer<Meta = ()> {
    pub name: String,
    pub role: LayerRole,
    pub side: Side,
    pub shape_start: u32,
    pub shape_count: u32,
    pub bbox: BBox,
    pub meta: Meta,
}

impl<Meta: Default> MaskLayer<Meta> {
    pub fn new(name: impl Into<String>, role: LayerRole, side: Side) -> Self {
        Self {
            name: name.into(),
            role,
            side,
            shape_start: 0,
            shape_count: 0,
            bbox: BBox::empty(),
            meta: Meta::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MaskShape {
    pub contour_start: u32,
    pub contour_count: u32,
    pub bbox: BBox,
    pub fill_rule: FillRule,
}

impl MaskShape {
    pub fn new(fill_rule: FillRule) -> Self {
        Self {
            contour_start: 0,
            contour_count: 0,
            bbox: BBox::empty(),
            fill_rule,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MaskContour {
    pub cmd_start: u32,
    pub cmd_count: u32,
    pub bbox: BBox,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_final_shapes_by_layer() {
        let mut doc = MaskDocument::<()>::new(Unit::Millimeter);
        let layer = doc.push_layer(MaskLayer::new("F.Cu", LayerRole::Copper, Side::Top));
        doc.push_shape(
            layer,
            MaskShape::new(FillRule::NonZero),
            vec![PathPayload {
                bbox: BBox::from_point(Point::new(0.0, 0.0)),
                cmds: vec![PathCmd::move_to(Point::new(0.0, 0.0)), PathCmd::close()],
            }],
        );

        assert_eq!(doc.layers[0].shape_count, 1);
        assert_eq!(doc.shapes[0].contour_count, 1);
        assert_eq!(doc.path_cmds.len(), 2);
        doc.validate().unwrap();
    }

    #[test]
    fn renders_full_circle_arc_as_two_svg_arcs() {
        let mut doc = MaskDocument::<()>::new(Unit::Millimeter);
        let layer = doc.push_layer(MaskLayer::new("F.Cu", LayerRole::Copper, Side::Top));
        doc.push_shape(
            layer,
            MaskShape::new(FillRule::NonZero),
            vec![PathPayload {
                bbox: BBox {
                    min: Point::new(-1.0, -1.0),
                    max: Point::new(1.0, 1.0),
                },
                cmds: vec![
                    PathCmd::move_to(Point::new(1.0, 0.0)),
                    PathCmd::arc_to(Point::new(1.0, 0.0), Point::new(0.0, 0.0), false),
                    PathCmd::close(),
                ],
            }],
        );

        let svg = render_svg(&doc, 0);

        assert_eq!(svg.matches(" A1 1 0 0 1 ").count(), 2);
        assert!(svg.contains("-1 0"));
    }

    #[test]
    fn renders_profile_layer_as_black_outline_overlay() {
        let mut doc = MaskDocument::<()>::new(Unit::Millimeter);
        let copper = doc.push_layer(MaskLayer::new("F.Cu", LayerRole::Copper, Side::Top));
        let profile = doc.push_layer(MaskLayer::new("Profile", LayerRole::Profile, Side::None));
        let contour = PathPayload {
            bbox: BBox {
                min: Point::new(0.0, 0.0),
                max: Point::new(1.0, 1.0),
            },
            cmds: vec![
                PathCmd::move_to(Point::new(0.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 0.0)),
                PathCmd::line_to(Point::new(1.0, 1.0)),
                PathCmd::close(),
            ],
        };
        doc.push_shape(
            copper,
            MaskShape::new(FillRule::NonZero),
            vec![contour.clone()],
        );
        doc.push_shape(profile, MaskShape::new(FillRule::NonZero), vec![contour]);

        let svg = render_svg_layers(&doc, &[copper as usize, profile as usize]);

        assert!(svg.contains("fill='#d87822'"));
        assert!(svg.contains("stroke='#000000'"));
        assert!(svg.contains("data-board-outline='true'"));
    }
}
