//! The STEP output buffer, the entity vocabulary written into it, and the
//! board solid built from that vocabulary.
//!
//! Every `#id` comes from [`Writer::id`]. Ids are handed out in emission
//! order except where a block is reserved first because entities refer
//! forward to each other.

use std::io::Write as _;

use crate::geom::{Transform, Vec2, Vec3};
use crate::holes::RoundHole;
use crate::outline::{Edge, Loop, Solid};

pub(crate) struct Writer {
    pub(crate) buf: Vec<u8>,
    pub(crate) next_id: u32,
}

/// The four ids an `AXIS2_PLACEMENT_3D` occupies.
#[derive(Clone, Copy)]
pub(crate) struct Placement {
    origin: u32,
    z: u32,
    x: u32,
    pub(crate) axis: u32,
}

impl Placement {
    pub(crate) fn reserve(w: &mut Writer) -> Self {
        Self {
            origin: w.id(),
            z: w.id(),
            x: w.id(),
            axis: w.id(),
        }
    }
}

pub(crate) fn push_uint(out: &mut Vec<u8>, mut value: u32) {
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

/// STEP real literal: fixed notation, trailing zeros trimmed, always with
/// a decimal point.
pub(crate) fn push_float(out: &mut Vec<u8>, value: f64) {
    // Nine decimals as an integer: coordinates are millimetres, so this
    // is a nanometre grid, and integer digits are far cheaper than
    // formatting a float exactly.
    const SCALE: f64 = 1e9;
    let scaled = (value * SCALE).round();
    if scaled.abs() >= 1e18 {
        write!(out, "{value:.9}").unwrap();
        return;
    }
    let mut n = scaled.abs() as u64;
    if scaled < 0.0 && n != 0 {
        out.push(b'-');
    }
    let whole = n / 1_000_000_000;
    n %= 1_000_000_000;
    push_u64(out, whole);
    out.push(b'.');
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut digits = [b'0'; 9];
    for d in digits.iter_mut().rev() {
        *d = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let mut len = 9;
    while digits[len - 1] == b'0' {
        len -= 1;
    }
    out.extend_from_slice(&digits[..len]);
}

fn push_u64(out: &mut Vec<u8>, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

/// Copy STEP text with every `#id` moved up by `offset`, so a block
/// written with ids from 1 can be placed anywhere in the file. Copper
/// text has no quoted `#`.
pub(crate) fn relocate_ids(text: &[u8], offset: u32, out: &mut Vec<u8>) {
    let mut copied = 0;
    let mut at = 0;
    while let Some(off) = memchr::memchr(b'#', &text[at..]) {
        at += off + 1;
        let start = at;
        let mut id: u32 = 0;
        while at < text.len() && text[at].is_ascii_digit() {
            id = id * 10 + (text[at] - b'0') as u32;
            at += 1;
        }
        if at > start {
            out.extend_from_slice(&text[copied..start]);
            push_uint(out, id + offset);
            copied = at;
        }
    }
    out.extend_from_slice(&text[copied..]);
}

impl Writer {
    pub(crate) fn new(next_id: u32) -> Self {
        Self {
            buf: Vec::new(),
            next_id,
        }
    }

    pub(crate) fn id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub(crate) fn text(&mut self, s: &str) {
        self.buf.extend_from_slice(s.as_bytes());
    }

    fn float(&mut self, v: f64) {
        push_float(&mut self.buf, v);
    }

    fn reference(&mut self, id: u32) {
        self.buf.push(b'#');
        push_uint(&mut self.buf, id);
    }

    fn refs(&mut self, ids: &[u32]) {
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                self.buf.push(b',');
            }
            self.reference(*id);
        }
    }

    fn quoted(&mut self, s: &str) {
        self.buf.push(b'\'');
        for b in s.bytes() {
            if b == b'\'' {
                self.buf.push(b'\'');
            }
            self.buf.push(b);
        }
        self.buf.push(b'\'');
    }

    fn begin(&mut self, id: u32, kind: &str) {
        self.reference(id);
        self.text(" = ");
        self.text(kind);
        self.buf.push(b'(');
    }

    fn open(&mut self, kind: &str) -> u32 {
        let id = self.id();
        self.begin(id, kind);
        id
    }

    fn end(&mut self) {
        self.text(");\n");
    }

    fn xyz(&mut self, v: Vec3) {
        self.buf.push(b'(');
        self.float(v.x);
        self.buf.push(b',');
        self.float(v.y);
        self.buf.push(b',');
        self.float(v.z);
        self.buf.push(b')');
    }

    pub(crate) fn cartesian_point(&mut self, p: Vec3) -> u32 {
        let id = self.open("CARTESIAN_POINT");
        self.text("'',");
        self.xyz(p);
        self.end();
        id
    }

    pub(crate) fn direction(&mut self, d: Vec3) -> u32 {
        let id = self.open("DIRECTION");
        self.text("'',");
        self.xyz(d.normalize_or_zero());
        self.end();
        id
    }

    fn vertex(&mut self, point: u32) -> u32 {
        let id = self.open("VERTEX_POINT");
        self.text("'',");
        self.reference(point);
        self.end();
        id
    }

    pub(crate) fn axis2(&mut self, origin: Vec3, z: Vec3, x: Vec3) -> u32 {
        let p = Placement::reserve(self);
        self.axis2_at(p, origin, z, x);
        p.axis
    }

    fn axis2_at(&mut self, p: Placement, origin: Vec3, z: Vec3, x: Vec3) {
        self.begin(p.origin, "CARTESIAN_POINT");
        self.text("'',");
        self.xyz(origin);
        self.end();
        for (id, d) in [(p.z, z), (p.x, x)] {
            self.begin(id, "DIRECTION");
            self.text("'',");
            self.xyz(d.normalize_or_zero());
            self.end();
        }
        self.begin(p.axis, "AXIS2_PLACEMENT_3D");
        self.text("'',");
        self.refs(&[p.origin, p.z, p.x]);
        self.end();
    }

    pub(crate) fn axis_placement_at(&mut self, p: Placement, t: &Transform) {
        self.axis2_at(p, t.origin(), t.direction(Vec3::Z), t.direction(Vec3::X));
    }

    pub(crate) fn axis_placement(&mut self, t: &Transform) -> u32 {
        let p = Placement::reserve(self);
        self.axis_placement_at(p, t);
        p.axis
    }

    fn plane(&mut self, origin: Vec3, normal: Vec3, x: Vec3) -> u32 {
        let placement = self.axis2(origin, normal, x);
        let id = self.open("PLANE");
        self.text("'',");
        self.reference(placement);
        self.end();
        id
    }

    fn cylinder(&mut self, center: Vec2, z: f64, radius: f64) -> u32 {
        let placement = self.axis2(center.extend(z), Vec3::Z, Vec3::X);
        let id = self.open("CYLINDRICAL_SURFACE");
        self.text("'',");
        self.reference(placement);
        self.buf.push(b',');
        self.float(radius);
        self.end();
        id
    }

    fn line(&mut self, from: Vec3, to: Vec3) -> u32 {
        let point = self.cartesian_point(from);
        let direction = self.direction(to - from);
        let vector = self.open("VECTOR");
        self.text("'',");
        self.reference(direction);
        self.buf.push(b',');
        self.float(from.distance(to));
        self.end();
        let id = self.open("LINE");
        self.text("'',");
        self.refs(&[point, vector]);
        self.end();
        id
    }

    fn circle(&mut self, center: Vec2, z: f64, radius: f64, ccw: bool) -> u32 {
        let axis = if ccw { Vec3::Z } else { Vec3::NEG_Z };
        let placement = self.axis2(center.extend(z), axis, Vec3::X);
        let id = self.open("CIRCLE");
        self.text("'',");
        self.reference(placement);
        self.buf.push(b',');
        self.float(radius);
        self.end();
        id
    }

    fn edge_curve(&mut self, v0: u32, v1: u32, curve: u32) -> u32 {
        let id = self.open("EDGE_CURVE");
        self.text("'',");
        self.refs(&[v0, v1, curve]);
        self.text(",.T.");
        self.end();
        id
    }

    /// `ADVANCED_FACE` over `surface`; the first bound is the outer one.
    fn face(&mut self, bounds: &[&[(u32, bool)]], surface: u32, same_sense: bool) -> u32 {
        let mut bound_ids = Vec::with_capacity(bounds.len());
        let mut oriented = Vec::new();
        for (index, edges) in bounds.iter().enumerate() {
            oriented.clear();
            for (edge, same) in edges.iter() {
                let id = self.open("ORIENTED_EDGE");
                self.text("'',*,*,");
                self.reference(*edge);
                self.text(if *same { ",.T." } else { ",.F." });
                self.end();
                oriented.push(id);
            }
            let edge_loop = self.open("EDGE_LOOP");
            self.text("'',(");
            self.refs(&oriented);
            self.buf.push(b')');
            self.end();
            let bound = self.open(if index == 0 {
                "FACE_OUTER_BOUND"
            } else {
                "FACE_BOUND"
            });
            self.text("'',");
            self.reference(edge_loop);
            self.text(",.T.");
            self.end();
            bound_ids.push(bound);
        }
        let id = self.open("ADVANCED_FACE");
        self.text("'',(");
        self.refs(&bound_ids);
        self.text("),");
        self.reference(surface);
        self.text(if same_sense { ",.T." } else { ",.F." });
        self.end();
        id
    }

    fn manifold_solid(&mut self, name: &str, faces: &[u32]) -> u32 {
        let shell = self.open("CLOSED_SHELL");
        self.text("'',(");
        self.refs(faces);
        self.buf.push(b')');
        self.end();
        let id = self.open("MANIFOLD_SOLID_BREP");
        self.quoted(name);
        self.buf.push(b',');
        self.reference(shell);
        self.end();
        id
    }

    pub(crate) fn shape_representation(&mut self, id: u32, items: &[u32], context: u32) {
        self.representation_at("SHAPE_REPRESENTATION", id, items, context);
    }

    pub(crate) fn brep_representation(&mut self, id: u32, items: &[u32], context: u32) {
        self.representation_at("ADVANCED_BREP_SHAPE_REPRESENTATION", id, items, context);
    }

    fn representation_at(&mut self, kind: &str, id: u32, items: &[u32], context: u32) {
        self.begin(id, kind);
        self.text("'',(");
        self.refs(items);
        self.text("),");
        self.reference(context);
        self.end();
    }

    pub(crate) fn representation_map_at(&mut self, id: u32, origin: u32, representation: u32) {
        self.begin(id, "REPRESENTATION_MAP");
        self.refs(&[origin, representation]);
        self.end();
    }

    pub(crate) fn mapped_item(&mut self, map: u32, target: u32) -> u32 {
        let id = self.open("MAPPED_ITEM");
        self.text("'',");
        self.refs(&[map, target]);
        self.end();
        id
    }

    /// Colour one solid the way OCCT's XCAF writer does, returning the
    /// `STYLED_ITEM` for the presentation representation.
    pub(crate) fn styled_solid(&mut self, solid: u32, rgb: [f64; 3]) -> u32 {
        let styled = self.id();
        let assignment = self.id();
        let usage = self.id();
        let side = self.id();
        let fill_area = self.id();
        let fill = self.id();
        let fill_colour = self.id();
        let colour = self.id();

        self.begin(styled, "STYLED_ITEM");
        self.text("'',(");
        self.reference(assignment);
        self.text("),");
        self.reference(solid);
        self.end();
        self.begin(assignment, "PRESENTATION_STYLE_ASSIGNMENT");
        self.buf.push(b'(');
        self.reference(usage);
        self.buf.push(b')');
        self.end();
        self.begin(usage, "SURFACE_STYLE_USAGE");
        self.text(".BOTH.,");
        self.reference(side);
        self.end();
        self.begin(side, "SURFACE_SIDE_STYLE");
        self.text("'',(");
        self.reference(fill_area);
        self.buf.push(b')');
        self.end();
        self.begin(fill_area, "SURFACE_STYLE_FILL_AREA");
        self.reference(fill);
        self.end();
        self.begin(fill, "FILL_AREA_STYLE");
        self.text("'',(");
        self.reference(fill_colour);
        self.buf.push(b')');
        self.end();
        self.begin(fill_colour, "FILL_AREA_STYLE_COLOUR");
        self.text("'',");
        self.reference(colour);
        self.end();
        self.begin(colour, "COLOUR_RGB");
        self.text("'',");
        self.float(rgb[0]);
        self.buf.push(b',');
        self.float(rgb[1]);
        self.buf.push(b',');
        self.float(rgb[2]);
        self.end();
        styled
    }

    /// A geometric context sharing the board's units with its own
    /// distance accuracy.
    pub(crate) fn geometry_context(&mut self, base: &Context, accuracy: f64) -> u32 {
        let uncertainty = self.id();
        let context = self.id();
        let (length, angle, solid) = (
            base.length_unit,
            base.plane_angle_unit,
            base.solid_angle_unit,
        );
        write!(
            self.buf,
            "#{context} = ( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{uncertainty})) GLOBAL_UNIT_ASSIGNED_CONTEXT((#{length},#{angle},#{solid})) REPRESENTATION_CONTEXT('','') );\n\
#{uncertainty} = UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE("
        )
        .unwrap();
        self.float(accuracy);
        write!(self.buf, "),#{length},'distance_accuracy_value','');").unwrap();
        self.buf.push(b'\n');
        context
    }

    /// A presentation style for a colour, shared by many styled items.
    pub(crate) fn style_assignment(&mut self, rgb: [f64; 3]) -> u32 {
        self.style_assignment_with(rgb, None)
    }

    /// A surface colour, optionally with a transparency in `[0, 1]`.
    pub(crate) fn style_assignment_with(
        &mut self,
        rgb: [f64; 3],
        transparency: Option<f64>,
    ) -> u32 {
        let assignment = self.id();
        let usage = self.id();
        let side = self.id();
        let fill_area = self.id();
        let fill = self.id();
        let fill_colour = self.id();
        let colour = self.id();
        let rendering = transparency.map(|t| (self.id(), self.id(), t));
        self.begin(assignment, "PRESENTATION_STYLE_ASSIGNMENT");
        self.buf.push(b'(');
        self.reference(usage);
        self.buf.push(b')');
        self.end();
        self.begin(usage, "SURFACE_STYLE_USAGE");
        self.text(".BOTH.,");
        self.reference(side);
        self.end();
        self.begin(side, "SURFACE_SIDE_STYLE");
        self.text("'',(");
        self.reference(fill_area);
        if let Some((rendering, _, _)) = rendering {
            self.buf.push(b',');
            self.reference(rendering);
        }
        self.buf.push(b')');
        self.end();
        if let Some((rendering, transparent, t)) = rendering {
            self.begin(rendering, "SURFACE_STYLE_RENDERING_WITH_PROPERTIES");
            self.text(".NORMAL_SHADING.,");
            self.reference(colour);
            self.text(",(");
            self.reference(transparent);
            self.text(")");
            self.end();
            self.begin(transparent, "SURFACE_STYLE_TRANSPARENT");
            self.float(t);
            self.end();
        }
        self.begin(fill_area, "SURFACE_STYLE_FILL_AREA");
        self.reference(fill);
        self.end();
        self.begin(fill, "FILL_AREA_STYLE");
        self.text("'',(");
        self.reference(fill_colour);
        self.buf.push(b')');
        self.end();
        self.begin(fill_colour, "FILL_AREA_STYLE_COLOUR");
        self.text("'',");
        self.reference(colour);
        self.end();
        self.begin(colour, "COLOUR_RGB");
        self.text("'',");
        self.float(rgb[0]);
        self.buf.push(b',');
        self.float(rgb[1]);
        self.buf.push(b',');
        self.float(rgb[2]);
        self.end();
        assignment
    }

    pub(crate) fn styled_item(&mut self, solid: u32, assignment: u32) -> u32 {
        let id = self.open("STYLED_ITEM");
        self.text("'',(");
        self.reference(assignment);
        self.text("),");
        self.reference(solid);
        self.end();
        id
    }

    pub(crate) fn presentation_representation(&mut self, styled: &[u32], context: u32) {
        self.open("MECHANICAL_DESIGN_GEOMETRIC_PRESENTATION_REPRESENTATION");
        self.text("'',(");
        self.refs(styled);
        self.text("),");
        self.reference(context);
        self.end();
    }
}

/// Top-level entities every product hangs off. Reserved before anything
/// is written and emitted last.
pub(crate) struct Root {
    pub(crate) app_context: u32,
    pub(crate) product_def: u32,
    pub(crate) shape_rep: u32,
    pub(crate) geom_context: u32,
    pub(crate) context: Context,
    ids: [u32; 19],
}

/// The unit entities every geometric context is built from.
#[derive(Clone, Copy)]
pub(crate) struct Context {
    length_unit: u32,
    plane_angle_unit: u32,
    solid_angle_unit: u32,
}

impl Root {
    pub(crate) fn reserve(w: &mut Writer) -> Self {
        let mut ids = [0u32; 19];
        for id in &mut ids {
            *id = w.id();
        }
        Self {
            app_context: ids[1],
            product_def: ids[4],
            shape_rep: ids[9],
            geom_context: ids[14],
            context: Context {
                length_unit: ids[15],
                plane_angle_unit: ids[16],
                solid_angle_unit: ids[17],
            },
            ids,
        }
    }

    /// `placements` are the occurrence axes placed in this assembly.
    pub(crate) fn emit(&self, w: &mut Writer, name: &str, placements: &[u32]) {
        let [
            app_protocol,
            app_context,
            shape_def_rep,
            product_def_shape,
            product_def,
            formation,
            product,
            product_context,
            product_def_context,
            shape_rep,
            origin,
            z_axis,
            x_axis,
            placement,
            geom_context,
            length_unit,
            plane_angle_unit,
            solid_angle_unit,
            uncertainty,
        ] = self.ids;
        let name = name.replace('\'', "''");
        write!(
            w.buf,
            "#{app_protocol} = APPLICATION_PROTOCOL_DEFINITION('international standard','ap242_managed_model_based_3d_engineering',2014,#{app_context});\n\
#{app_context} = APPLICATION_CONTEXT('core data for automotive mechanical design processes');\n\
#{shape_def_rep} = SHAPE_DEFINITION_REPRESENTATION(#{product_def_shape},#{shape_rep});\n\
#{product_def_shape} = PRODUCT_DEFINITION_SHAPE('','',#{product_def});\n\
#{product_def} = PRODUCT_DEFINITION('design','',#{formation},#{product_def_context});\n\
#{formation} = PRODUCT_DEFINITION_FORMATION('','',#{product});\n\
#{product} = PRODUCT('{name}','{name}','',(#{product_context}));\n\
#{product_context} = PRODUCT_CONTEXT('',#{app_context},'mechanical');\n\
#{product_def_context} = PRODUCT_DEFINITION_CONTEXT('part definition',#{app_context},'design');\n\
#{shape_rep} = SHAPE_REPRESENTATION('',(#{placement}"
        )
        .unwrap();
        for id in placements {
            w.buf.push(b',');
            w.reference(*id);
        }
        write!(
            w.buf,
            "),#{geom_context});\n\
#{origin} = CARTESIAN_POINT('',(0.0,0.0,0.0));\n\
#{z_axis} = DIRECTION('',(0.0,0.0,1.0));\n\
#{x_axis} = DIRECTION('',(1.0,0.0,0.0));\n\
#{placement} = AXIS2_PLACEMENT_3D('',#{origin},#{z_axis},#{x_axis});\n\
#{geom_context} = ( GEOMETRIC_REPRESENTATION_CONTEXT(3) GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{uncertainty})) GLOBAL_UNIT_ASSIGNED_CONTEXT((#{length_unit},#{plane_angle_unit},#{solid_angle_unit})) REPRESENTATION_CONTEXT('','') );\n\
#{uncertainty} = UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-06),#{length_unit},'distance_accuracy_value','');\n\
#{length_unit} = ( LENGTH_UNIT() NAMED_UNIT(*) SI_UNIT(.MILLI.,.METRE.) );\n\
#{plane_angle_unit} = ( NAMED_UNIT(*) PLANE_ANGLE_UNIT() SI_UNIT($,.RADIAN.) );\n\
#{solid_angle_unit} = ( NAMED_UNIT(*) SI_UNIT($,.STERADIAN.) SOLID_ANGLE_UNIT() );\n"
        )
        .unwrap();
    }
}

/// A part definition, shared by all of its occurrences.
#[derive(Clone, Copy)]
pub(crate) struct Part {
    product_def: u32,
    representation: u32,
    origin: u32,
}

impl Writer {
    pub(crate) fn part(
        &mut self,
        root: &Root,
        name: &str,
        representation: u32,
        origin: u32,
    ) -> Part {
        let ids: [u32; 8] = std::array::from_fn(|_| self.id());
        let [
            shape_def_rep,
            product_def_shape,
            product_def,
            formation,
            product,
            product_context,
            product_def_context,
            category,
        ] = ids;
        let name = name.replace('\'', "''");
        let app_context = root.app_context;
        write!(
            self.buf,
            "#{shape_def_rep} = SHAPE_DEFINITION_REPRESENTATION(#{product_def_shape},#{representation});\n\
#{product_def_shape} = PRODUCT_DEFINITION_SHAPE('','',#{product_def});\n\
#{product_def} = PRODUCT_DEFINITION('design','',#{formation},#{product_def_context});\n\
#{formation} = PRODUCT_DEFINITION_FORMATION('','',#{product});\n\
#{product} = PRODUCT('{name}','{name}','',(#{product_context}));\n\
#{product_context} = PRODUCT_CONTEXT('',#{app_context},'mechanical');\n\
#{product_def_context} = PRODUCT_DEFINITION_CONTEXT('part definition',#{app_context},'design');\n\
#{category} = PRODUCT_RELATED_PRODUCT_CATEGORY('part',$,(#{product}));\n"
        )
        .unwrap();
        Part {
            product_def,
            representation,
            origin,
        }
    }

    /// Place one occurrence of `part` in the root assembly, returning the
    /// placement axis the root representation must list.
    pub(crate) fn occurrence(
        &mut self,
        root: &Root,
        part: Part,
        index: usize,
        name: &str,
        transform: &Transform,
    ) -> u32 {
        let placement = Placement::reserve(self);
        let ids: [u32; 5] = std::array::from_fn(|_| self.id());
        let [
            item_transform,
            relationship,
            product_def_shape,
            usage,
            context_dependent,
        ] = ids;
        let origin = transform.origin();
        let x = transform.direction(Vec3::X);
        let z = transform.direction(Vec3::Z);
        self.begin(placement.origin, "CARTESIAN_POINT");
        self.text("'',");
        self.xyz(origin);
        self.end();
        self.begin(placement.z, "DIRECTION");
        self.text("'',");
        self.xyz(z);
        self.end();
        self.begin(placement.x, "DIRECTION");
        self.text("'',");
        self.xyz(x);
        self.end();
        self.begin(placement.axis, "AXIS2_PLACEMENT_3D");
        self.quoted(name);
        self.buf.push(b',');
        self.refs(&[placement.origin, placement.z, placement.x]);
        self.end();

        let name = name.replace('\'', "''");
        let root_shape_rep = root.shape_rep;
        let root_product_def = root.product_def;
        let part_product_def = part.product_def;
        let part_origin = part.origin;
        let part_representation = part.representation;
        let axis = placement.axis;
        write!(
            self.buf,
            "#{item_transform} = ITEM_DEFINED_TRANSFORMATION('','',#{part_origin},#{axis});\n\
#{relationship} = ( REPRESENTATION_RELATIONSHIP('','',#{part_representation},#{root_shape_rep}) REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(#{item_transform}) SHAPE_REPRESENTATION_RELATIONSHIP() );\n\
#{product_def_shape} = PRODUCT_DEFINITION_SHAPE('Placement','Placement of an item',#{usage});\n\
#{usage} = NEXT_ASSEMBLY_USAGE_OCCURRENCE('{index}','{name}','',#{root_product_def},#{part_product_def},$);\n\
#{context_dependent} = CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(#{relationship},#{product_def_shape});\n"
        )
        .unwrap();
        axis
    }
}

/// Vertices and edges of one loop extruded between two heights.
struct RoundHoleBounds {
    top: Option<u32>,
    bottom: Option<u32>,
}

struct Ring {
    top: Vec<u32>,
    bottom: Vec<u32>,
    /// Vertical edge at the start of each loop edge, top to bottom.
    vertical: Vec<u32>,
}

impl Writer {
    fn ring(&mut self, edges: &[Edge], z0: f64, z1: f64) -> Ring {
        let n = edges.len();
        let mut top_vertex = Vec::with_capacity(n);
        let mut bottom_vertex = Vec::with_capacity(n);
        for edge in edges {
            let p = edge.start();
            let top = self.cartesian_point(p.extend(z1));
            top_vertex.push(self.vertex(top));
            let bottom = self.cartesian_point(p.extend(z0));
            bottom_vertex.push(self.vertex(bottom));
        }
        let mut ring = Ring {
            top: Vec::with_capacity(n),
            bottom: Vec::with_capacity(n),
            vertical: Vec::with_capacity(n),
        };
        for (i, edge) in edges.iter().enumerate() {
            let next = (i + 1) % n;
            for (z, vertices, out) in [
                (z1, &top_vertex, &mut ring.top),
                (z0, &bottom_vertex, &mut ring.bottom),
            ] {
                let curve = match *edge {
                    Edge::Line { a, b } => self.line(a.extend(z), b.extend(z)),
                    Edge::Arc { c, ccw, .. } => self.circle(c, z, edge.radius(), ccw),
                };
                out.push(self.edge_curve(vertices[i], vertices[next], curve));
            }
            let p = edge.start();
            let line = self.line(p.extend(z1), p.extend(z0));
            ring.vertical
                .push(self.edge_curve(top_vertex[i], bottom_vertex[i], line));
        }
        ring
    }

    /// One wall per edge. Faces keep their normal pointing away from the
    /// material, which for a counter-clockwise outer loop and clockwise
    /// holes is always to the right of the direction of travel.
    fn walls(&mut self, edges: &[Edge], ring: &Ring, z0: f64, faces: &mut Vec<u32>) {
        let n = edges.len();
        for (i, edge) in edges.iter().enumerate() {
            let next = (i + 1) % n;
            let (surface, same_sense) = match *edge {
                Edge::Line { a, b } => {
                    let d = b - a;
                    (
                        self.plane(a.extend(0.0), Vec3::new(d.y, -d.x, 0.0), Vec3::Z),
                        true,
                    )
                }
                Edge::Arc { c, ccw, .. } => (self.cylinder(c, z0, edge.radius()), ccw),
            };
            let bound = [
                (ring.top[i], true),
                (ring.vertical[next], true),
                (ring.bottom[i], false),
                (ring.vertical[i], false),
            ];
            faces.push(self.face(&[&bound], surface, same_sense));
        }
    }

    /// Faces of one round hole, plus its bounds on the two cap faces.
    ///
    /// The profile runs top down. A hole is material-outside geometry, so
    /// every wall keeps its normal towards the axis and every shoulder
    /// faces into the void.
    fn round_hole(&mut self, hole: &RoundHole, faces: &mut Vec<u32>) -> RoundHoleBounds {
        let c = hole.center;
        let profile = &hole.profile;
        // A circle edge and its seam vertex at every profile point with a
        // radius; points at radius zero are the centre of a floor disc.
        let mut circles: Vec<Option<(u32, u32)>> = Vec::with_capacity(profile.len());
        for &(z, r) in profile {
            if r <= 0.0 {
                circles.push(None);
                continue;
            }
            let point = self.cartesian_point(Vec3::new(c.x + r, c.y, z));
            let vertex = self.vertex(point);
            let curve = self.circle(c, z, r, false);
            circles.push(Some((self.edge_curve(vertex, vertex, curve), vertex)));
        }
        for i in 0..profile.len() - 1 {
            let (z0, r0) = profile[i];
            let (z1, r1) = profile[i + 1];
            let flat = (z0 - z1).abs() < 1e-9;
            if flat {
                // Shoulder between two radii, or a floor disc.
                let (wide, narrow, normal) = if r0 > r1 {
                    (circles[i], circles[i + 1], Vec3::Z)
                } else {
                    (circles[i + 1], circles[i], Vec3::NEG_Z)
                };
                let Some((outer, _)) = wide else {
                    continue;
                };
                let plane = self.plane(Vec3::new(0.0, 0.0, z0), normal, Vec3::X);
                // Circles are wound clockwise; the outer bound must run
                // counter-clockwise about the normal, the inner the other way.
                let up = normal.z > 0.0;
                let outer_bound = [(outer, !up)];
                match narrow {
                    Some((inner, _)) => {
                        let inner_bound = [(inner, up)];
                        faces.push(self.face(&[&outer_bound, &inner_bound], plane, true));
                    }
                    None => faces.push(self.face(&[&outer_bound], plane, true)),
                }
                continue;
            }
            let (Some((top, v_top)), Some((bottom, v_bottom))) = (circles[i], circles[i + 1])
            else {
                continue;
            };
            let seam_line = self.line(Vec3::new(c.x + r0, c.y, z0), Vec3::new(c.x + r1, c.y, z1));
            let seam = self.edge_curve(v_top, v_bottom, seam_line);
            let surface = if (r0 - r1).abs() < 1e-9 {
                self.cylinder(c, z1, r0)
            } else {
                // Cone opening along its axis, so the semi-angle is positive.
                let (origin_z, radius, axis) = if r0 > r1 {
                    (z1, r1, Vec3::Z)
                } else {
                    (z0, r0, Vec3::NEG_Z)
                };
                let semi_angle = ((r0 - r1).abs() / (z0 - z1)).atan();
                self.cone(c, origin_z, radius, axis, semi_angle)
            };
            let bound = [(top, true), (seam, true), (bottom, false), (seam, false)];
            faces.push(self.face(&[&bound], surface, false));
        }
        RoundHoleBounds {
            top: circles.first().copied().flatten().map(|(edge, _)| edge),
            bottom: circles.last().copied().flatten().map(|(edge, _)| edge),
        }
    }

    fn cone(&mut self, center: Vec2, z: f64, radius: f64, axis: Vec3, semi_angle: f64) -> u32 {
        let placement = self.axis2(center.extend(z), axis, Vec3::X);
        let id = self.open("CONICAL_SURFACE");
        self.text("'',");
        self.reference(placement);
        self.buf.push(b',');
        self.float(radius);
        self.buf.push(b',');
        self.float(semi_angle);
        self.end();
        id
    }

    /// A flat face at height `z` bounded by `outer` and `holes`, facing up
    /// or down, as a shell-based surface model. A downward face walks its
    /// loops the other way round.
    pub(crate) fn flat_face(&mut self, outer: &Loop, holes: &[Loop], z: f64, up: bool) -> u32 {
        let mut bounds: Vec<Vec<(u32, bool)>> = Vec::with_capacity(1 + holes.len());
        for l in std::iter::once(outer).chain(holes) {
            let reversed;
            let l = if up {
                l
            } else {
                reversed = l.reversed();
                &reversed
            };
            let n = l.edges.len();
            let mut vertices = Vec::with_capacity(n);
            for edge in &l.edges {
                let p = self.cartesian_point(edge.start().extend(z));
                vertices.push(self.vertex(p));
            }
            let mut loop_edges = Vec::with_capacity(n);
            for (i, edge) in l.edges.iter().enumerate() {
                let curve = match *edge {
                    Edge::Line { a, b } => self.line(a.extend(z), b.extend(z)),
                    Edge::Arc { c, ccw, .. } => self.circle(c, z, edge.radius(), ccw),
                };
                loop_edges.push((
                    self.edge_curve(vertices[i], vertices[(i + 1) % n], curve),
                    up,
                ));
            }
            bounds.push(loop_edges);
        }
        let plane = self.plane(
            Vec3::new(0.0, 0.0, z),
            if up { Vec3::Z } else { Vec3::NEG_Z },
            Vec3::X,
        );
        let refs: Vec<&[(u32, bool)]> = bounds.iter().map(Vec::as_slice).collect();
        let face = self.face(&refs, plane, true);
        let shell = self.open("OPEN_SHELL");
        self.text("'',(");
        self.reference(face);
        self.text(")");
        self.end();
        let model = self.open("SHELL_BASED_SURFACE_MODEL");
        self.text("'',(");
        self.reference(shell);
        self.text(")");
        self.end();
        model
    }

    /// Extrude a solid between `z0` and `z1` as one closed shell.
    pub(crate) fn solid(&mut self, name: &str, solid: &Solid, z0: f64, z1: f64) -> u32 {
        let forward = |ids: &[u32]| ids.iter().map(|id| (*id, true)).collect::<Vec<_>>();
        let backward = |ids: &[u32]| ids.iter().rev().map(|id| (*id, false)).collect::<Vec<_>>();

        let outer = self.ring(&solid.outer.edges, z0, z1);
        let holes: Vec<Ring> = solid
            .holes
            .iter()
            .map(|h| self.ring(&h.edges, z0, z1))
            .collect();
        let mut faces = Vec::new();
        let mut round_bounds = Vec::with_capacity(solid.round.len());
        for hole in &solid.round {
            round_bounds.push(self.round_hole(hole, &mut faces));
        }

        let mut top_bounds = vec![forward(&outer.top)];
        let mut bottom_bounds = vec![backward(&outer.bottom)];
        for hole in &holes {
            top_bounds.push(forward(&hole.top));
            bottom_bounds.push(backward(&hole.bottom));
        }
        for (hole, bounds) in solid.round.iter().zip(&round_bounds) {
            if let Some(edge) = bounds.top
                && hole.profile[0].0 >= z1 - 1e-9
            {
                top_bounds.push(vec![(edge, true)]);
            }
            if let Some(edge) = bounds.bottom
                && hole.profile.last().unwrap().0 <= z0 + 1e-9
            {
                bottom_bounds.push(vec![(edge, false)]);
            }
        }

        let top_plane = self.plane(Vec3::new(0.0, 0.0, z1), Vec3::Z, Vec3::X);
        let top_refs: Vec<&[(u32, bool)]> = top_bounds.iter().map(Vec::as_slice).collect();
        faces.push(self.face(&top_refs, top_plane, true));
        let bottom_plane = self.plane(Vec3::new(0.0, 0.0, z0), Vec3::NEG_Z, Vec3::X);
        let bottom_refs: Vec<&[(u32, bool)]> = bottom_bounds.iter().map(Vec::as_slice).collect();
        faces.push(self.face(&bottom_refs, bottom_plane, true));

        self.walls(&solid.outer.edges, &outer, z0, &mut faces);
        for (hole, ring) in solid.holes.iter().zip(&holes) {
            self.walls(&hole.edges, ring, z0, &mut faces);
        }
        self.manifold_solid(name, &faces)
    }
}
