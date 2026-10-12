//! VRML 2.0 models (`.wrl`), KiCad's own 3D model format, read to triangles.
//!
//! Supported: `Group`, `Transform`, `Switch`, `Shape` with `Appearance` and
//! `Material`, `IndexedFaceSet` (with `creaseAngle` and `ccw`; normals are
//! computed), `Box`, and `DEF`/`USE`: what KiCad's library and model
//! generators write. Other geometry, colour nodes, VRML 1.0, `Inline`,
//! `PROTO` and textures are reported as not read. Lines and points are
//! ignored, as KiCad ignores them.
//!
//! Lengths are in VRML units, which KiCad draws as 2.54 mm (0.1 inch) each.
//! [`parse`] returns triangles in millimetres.

use std::collections::HashMap;
use std::rc::Rc;

use glam::{DMat3, DMat4, DQuat, DVec3};

use crate::mesh::Primitive;

/// KiCad's VRML unit, 0.1 inch.
pub(crate) const MM_PER_UNIT: f64 = 2.54;

pub(crate) type Colored = Vec<([f32; 4], Primitive)>;

pub(crate) struct Parsed {
    pub(crate) primitives: Colored,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    Number(f64),
    Str(String),
    Open,
    Close,
    OpenBracket,
    CloseBracket,
}

fn tokenize(text: &str) -> Result<Vec<Token>, String> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            c if c.is_ascii_whitespace() || c == b',' => i += 1,
            b'#' => {
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
            }
            b'{' => {
                out.push(Token::Open);
                i += 1;
            }
            b'}' => {
                out.push(Token::Close);
                i += 1;
            }
            b'[' => {
                out.push(Token::OpenBracket);
                i += 1;
            }
            b']' => {
                out.push(Token::CloseBracket);
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                i = start;
                while i < b.len() && b[i] != b'"' {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i >= b.len() {
                    return Err("unterminated string".to_owned());
                }
                out.push(Token::Str(text[start..i].to_owned()));
                i += 1;
            }
            c if c == b'-' || c == b'+' || c == b'.' || c.is_ascii_digit() => {
                let start = i;
                i += 1;
                while i < b.len()
                    && (b[i].is_ascii_digit()
                        || matches!(b[i], b'.' | b'e' | b'E')
                        || ((b[i] == b'-' || b[i] == b'+') && matches!(b[i - 1], b'e' | b'E')))
                {
                    i += 1;
                }
                let s = &text[start..i];
                match s.parse::<f64>() {
                    Ok(v) => out.push(Token::Number(v)),
                    Err(_) => return Err(format!("bad number {s:?}")),
                }
            }
            _ => {
                let start = i;
                while i < b.len()
                    && !b[i].is_ascii_whitespace()
                    && !matches!(b[i], b',' | b'{' | b'}' | b'[' | b']' | b'"' | b'#')
                {
                    i += 1;
                }
                if i == start {
                    return Err("unexpected character".to_owned());
                }
                out.push(Token::Word(text[start..i].to_owned()));
            }
        }
    }
    Ok(out)
}

#[derive(Debug)]
enum Value {
    Numbers(Vec<f64>),
    Words(Vec<String>),
    Nodes(Vec<Rc<Node>>),
}

#[derive(Debug)]
struct Node {
    kind: String,
    fields: Vec<(String, Value)>,
}

impl Node {
    fn field(&self, name: &str) -> Option<&Value> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, v)| v)
    }

    fn numbers(&self, name: &str) -> Option<&[f64]> {
        match self.field(name)? {
            Value::Numbers(n) => Some(n),
            _ => None,
        }
    }

    fn number(&self, name: &str, default: f64) -> f64 {
        self.numbers(name)
            .and_then(|n| n.first().copied())
            .unwrap_or(default)
    }

    fn boolean(&self, name: &str, default: bool) -> bool {
        match self.field(name) {
            Some(Value::Words(w)) => match w.first().map(String::as_str) {
                Some("TRUE") => true,
                Some("FALSE") => false,
                _ => default,
            },
            _ => default,
        }
    }

    fn node(&self, name: &str) -> Option<&Rc<Node>> {
        match self.field(name)? {
            Value::Nodes(n) => n.first(),
            _ => None,
        }
    }

    fn children(&self, name: &str) -> &[Rc<Node>] {
        match self.field(name) {
            Some(Value::Nodes(n)) => n,
            _ => &[],
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
    defs: HashMap<String, Rc<Node>>,
    notes: Vec<String>,
    depth: usize,
}

const MAX_DEPTH: usize = 256;
/// `USE` lets a small file expand without bound.
const MAX_VISITS: usize = 2_000_000;
const MAX_TRIANGLES: usize = 5_000_000;
/// Larger polygons are fanned: ear clipping is cubic.
const MAX_EAR_CLIP: usize = 256;

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.at).cloned();
        self.at += 1;
        t
    }

    fn note(&mut self, note: String) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    fn skip_value(&mut self) {
        let mut depth = 0i32;
        while let Some(t) = self.next() {
            match t {
                Token::Open | Token::OpenBracket => depth += 1,
                Token::Close | Token::CloseBracket => depth -= 1,
                _ => {}
            }
            if depth <= 0 {
                break;
            }
        }
    }

    /// `DEF n T {}`, `USE n` or `T {}`; `None` for `NULL`.
    fn node(&mut self) -> Result<Option<Rc<Node>>, String> {
        let Some(Token::Word(word)) = self.next() else {
            return Err("expected a node".to_owned());
        };
        match word.as_str() {
            "NULL" => Ok(None),
            "USE" => match self.next() {
                Some(Token::Word(name)) => self
                    .defs
                    .get(&name)
                    .cloned()
                    .map(Some)
                    .ok_or_else(|| format!("USE of undefined {name}")),
                _ => Err("USE without a name".to_owned()),
            },
            "DEF" => {
                let Some(Token::Word(name)) = self.next() else {
                    return Err("DEF without a name".to_owned());
                };
                let node = self.node()?;
                if let Some(node) = &node {
                    self.defs.insert(name, node.clone());
                }
                Ok(node)
            }
            kind => {
                if self.next() != Some(Token::Open) {
                    return Err(format!("expected {{ after {kind}"));
                }
                self.depth += 1;
                if self.depth > MAX_DEPTH {
                    return Err("nodes nested too deeply".to_owned());
                }
                let mut fields = Vec::new();
                loop {
                    match self.peek() {
                        Some(Token::Close) => {
                            self.at += 1;
                            break;
                        }
                        Some(Token::Word(name)) => {
                            let name = name.clone();
                            if matches!(name.as_str(), "ROUTE") {
                                self.at += 4;
                                continue;
                            }
                            self.at += 1;
                            let value = self.value()?;
                            fields.push((name, value));
                        }
                        _ => return Err(format!("unexpected token in {kind}")),
                    }
                }
                self.depth -= 1;
                Ok(Some(Rc::new(Node {
                    kind: kind.to_owned(),
                    fields,
                })))
            }
        }
    }

    fn is_node_start(&self) -> bool {
        match self.peek() {
            Some(Token::Word(w)) => match w.as_str() {
                "DEF" | "USE" | "NULL" => true,
                "TRUE" | "FALSE" => false,
                _ => self.tokens.get(self.at + 1) == Some(&Token::Open),
            },
            _ => false,
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.peek() {
            Some(Token::OpenBracket) => {
                self.at += 1;
                let mut numbers = Vec::new();
                let mut words = Vec::new();
                let mut nodes = Vec::new();
                loop {
                    match self.peek() {
                        Some(Token::CloseBracket) => {
                            self.at += 1;
                            break;
                        }
                        Some(Token::Number(n)) => {
                            numbers.push(*n);
                            self.at += 1;
                        }
                        Some(Token::Str(s)) => {
                            words.push(s.clone());
                            self.at += 1;
                        }
                        Some(Token::Word(_)) if self.is_node_start() => {
                            if let Some(node) = self.node()? {
                                nodes.push(node);
                            }
                        }
                        Some(Token::Word(w)) => {
                            words.push(w.clone());
                            self.at += 1;
                        }
                        _ => return Err("unterminated list".to_owned()),
                    }
                }
                Ok(if !nodes.is_empty() {
                    Value::Nodes(nodes)
                } else if !numbers.is_empty() {
                    Value::Numbers(numbers)
                } else {
                    Value::Words(words)
                })
            }
            Some(Token::Number(_)) => {
                let mut numbers = Vec::new();
                while let Some(Token::Number(n)) = self.peek() {
                    numbers.push(*n);
                    self.at += 1;
                }
                Ok(Value::Numbers(numbers))
            }
            Some(Token::Str(s)) => {
                let s = s.clone();
                self.at += 1;
                Ok(Value::Words(vec![s]))
            }
            Some(Token::Word(_)) if self.is_node_start() => {
                Ok(Value::Nodes(self.node()?.into_iter().collect()))
            }
            Some(Token::Word(w)) => {
                let w = w.clone();
                self.at += 1;
                Ok(Value::Words(vec![w]))
            }
            Some(Token::Open) => {
                self.skip_value();
                Ok(Value::Words(Vec::new()))
            }
            _ => Err("missing field value".to_owned()),
        }
    }

    fn file(&mut self) -> Result<Vec<Rc<Node>>, String> {
        let mut roots = Vec::new();
        while let Some(token) = self.peek() {
            match token {
                Token::Word(w) if w == "PROTO" || w == "EXTERNPROTO" => {
                    let w = w.clone();
                    self.note(format!("{w} definitions are not read"));
                    self.at += 2;
                    self.skip_value(); // interface [ ... ]
                    if w == "PROTO" {
                        self.skip_value(); // body { ... }
                    } else {
                        self.skip_value(); // url
                    }
                }
                Token::Word(w) if w == "ROUTE" => self.at += 4,
                Token::Word(_) => {
                    if let Some(node) = self.node()? {
                        roots.push(node);
                    }
                }
                _ => return Err("unexpected token at top level".to_owned()),
            }
        }
        Ok(roots)
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Parsed, String> {
    let text = String::from_utf8_lossy(bytes);
    let head = text.trim_start_matches('\u{feff}');
    if !head.starts_with("#VRML V2.0") {
        return Err(match head.get(..10) {
            Some(h) if h.starts_with("#VRML V1.0") => "VRML 1.0 is not supported".to_owned(),
            _ => "not a VRML 2.0 file (missing #VRML V2.0 header)".to_owned(),
        });
    }
    let mut parser = Parser {
        tokens: tokenize(head)?,
        at: 0,
        defs: HashMap::new(),
        notes: Vec::new(),
        depth: 0,
    };
    let roots = parser.file()?;
    let mut out = Builder {
        groups: HashMap::new(),
        notes: parser.notes,
        triangles: 0,
        visits: 0,
    };
    for root in &roots {
        out.visit(root, DMat4::from_scale(DVec3::splat(MM_PER_UNIT)), 0);
    }
    if out.visits > MAX_VISITS || out.triangles > MAX_TRIANGLES {
        return Err("model is too complex to read".to_owned());
    }
    let mut primitives: Colored = out
        .groups
        .into_iter()
        .map(|(key, p)| (key.map(|c| c as f32 / 255.0), p))
        .filter(|(_, p)| !p.is_empty())
        .collect();
    primitives.sort_by(|a, b| a.0.map(f32::to_bits).cmp(&b.0.map(f32::to_bits)));
    Ok(Parsed {
        primitives,
        notes: out.notes,
    })
}

struct Builder {
    groups: HashMap<[u16; 4], Primitive>,
    notes: Vec<String>,
    triangles: usize,
    visits: usize,
}

#[derive(Clone, Copy)]
struct Look {
    rgb: [f64; 3],
    alpha: f64,
}

const DEFAULT_LOOK: Look = Look {
    rgb: [0.8, 0.8, 0.8],
    alpha: 1.0,
};

impl Builder {
    fn note(&mut self, note: String) {
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
    }

    fn visit(&mut self, node: &Rc<Node>, m: DMat4, depth: usize) {
        self.visits += 1;
        if depth > MAX_DEPTH || self.visits > MAX_VISITS || self.triangles > MAX_TRIANGLES {
            return;
        }
        match node.kind.as_str() {
            "Group" | "Anchor" | "Collision" => {
                for child in node.children("children") {
                    self.visit(child, m, depth + 1);
                }
            }
            "Transform" => {
                let t = transform_of(node);
                for child in node.children("children") {
                    self.visit(child, m * t, depth + 1);
                }
            }
            "Switch" => {
                let choice = node.number("whichChoice", -1.0);
                if choice >= 0.0
                    && let Some(child) = node.children("choice").get(choice as usize)
                {
                    self.visit(child, m, depth + 1);
                }
            }
            "Shape" => self.shape(node, m),
            "Inline" => self.note("Inline files are not read".to_owned()),
            "Script"
            | "TimeSensor"
            | "PositionInterpolator"
            | "OrientationInterpolator"
            | "NavigationInfo"
            | "Viewpoint"
            | "Background"
            | "WorldInfo"
            | "DirectionalLight"
            | "PointLight"
            | "SpotLight"
            | "Fog" => {}
            "LOD" => {
                if let Some(child) = node.children("level").first() {
                    self.visit(child, m, depth + 1);
                }
            }
            other => self.note(format!("{other} nodes are not read")),
        }
    }

    fn shape(&mut self, shape: &Node, m: DMat4) {
        let look = shape
            .node("appearance")
            .and_then(|a| {
                if a.node("texture").is_some() {
                    self.note("textures are not read".to_owned());
                }
                a.node("material")
            })
            .map_or(DEFAULT_LOOK, |mat| Look {
                rgb: color3(mat.numbers("diffuseColor"), DEFAULT_LOOK.rgb),
                alpha: (1.0 - mat.number("transparency", 0.0)).clamp(0.0, 1.0),
            });
        let Some(geometry) = shape.node("geometry") else {
            return;
        };
        match geometry.kind.as_str() {
            "IndexedFaceSet" => self.face_set(geometry, look, m),
            "Box" => {
                let size = match geometry.numbers("size") {
                    Some([x, y, z, ..]) => DVec3::new(*x, *y, *z),
                    _ => DVec3::splat(2.0),
                };
                self.unit_box(look, m * DMat4::from_scale(size));
            }
            "IndexedLineSet" | "PointSet" => {}
            other => self.note(format!("{other} geometry is not read")),
        }
    }

    fn primitive(&mut self, look: Look) -> &mut Primitive {
        let rgb = look.rgb;
        let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u16;
        self.groups
            .entry([q(rgb[0]), q(rgb[1]), q(rgb[2]), q(look.alpha)])
            .or_default()
    }

    fn unit_box(&mut self, look: Look, m: DMat4) {
        let normal_matrix = DMat3::from_mat4(m).inverse().transpose();
        let cube = Primitive::unit_cube();
        let primitive = self.primitive(look);
        let base = primitive.positions.len() as u32;
        for (p, n) in cube.positions.iter().zip(&cube.normals) {
            let p = m.transform_point3(DVec3::from(p.map(f64::from)));
            let n = normal_matrix * DVec3::from(n.map(f64::from));
            primitive.vertex(p, n.normalize_or_zero());
        }
        let mirrored = m.determinant() < 0.0;
        for t in cube.indices.as_chunks::<3>().0 {
            let [a, b, c] = t.map(|i| base + i);
            primitive
                .indices
                .extend(if mirrored { [a, c, b] } else { [a, b, c] });
        }
        self.triangles += cube.indices.len() / 3;
    }

    /// Normals are smoothed across faces within the crease angle.
    fn face_set(&mut self, set: &Node, look: Look, m: DMat4) {
        let (Some(points), Some(index)) = (
            set.node("coord").and_then(|c| c.numbers("point")),
            set.numbers("coordIndex"),
        ) else {
            return;
        };
        if set.node("color").is_some() {
            self.note("per-face and per-vertex colours are not read".to_owned());
        }
        let points: Vec<DVec3> = points
            .as_chunks::<3>()
            .0
            .iter()
            .map(|c| m.transform_point3(DVec3::new(c[0], c[1], c[2])))
            .collect();
        // A transform that mirrors turns every face inside out.
        let flip = (m.determinant() < 0.0) == set.boolean("ccw", true);
        let faces: Vec<Vec<usize>> = index
            .split(|&i| i < 0.0)
            .filter(|f| f.len() >= 3)
            .map(|f| f.iter().map(|&i| i as usize).collect::<Vec<_>>())
            .filter(|f| f.iter().all(|&i| i < points.len()))
            .collect();
        let face_normals: Vec<DVec3> = faces
            .iter()
            .map(|f| {
                let n = newell(f.iter().map(|&i| points[i]));
                if flip { -n } else { n }.normalize_or_zero()
            })
            .collect();
        let crease = set.number("creaseAngle", 0.0);
        let mut at_point: HashMap<usize, Vec<usize>> = HashMap::new();
        if crease > 0.0 {
            for (fi, f) in faces.iter().enumerate() {
                for &i in f {
                    at_point.entry(i).or_default().push(fi);
                }
            }
        }
        self.triangles += faces.iter().map(|f| f.len() - 2).sum::<usize>();
        let primitive = self.primitive(look);
        for (fi, f) in faces.iter().enumerate() {
            let own = face_normals[fi];
            let corners: Vec<u32> = f
                .iter()
                .map(|i| {
                    let smooth: DVec3 = at_point
                        .get(i)
                        .into_iter()
                        .flatten()
                        .map(|&other| face_normals[other])
                        .filter(|n| n.dot(own) >= crease.cos())
                        .sum();
                    let n = smooth.try_normalize().unwrap_or(own);
                    primitive.vertex(points[*i], n)
                })
                .collect();
            for [a, b, c] in triangulate(f, &points) {
                let (b, c) = if flip { (c, b) } else { (b, c) };
                primitive
                    .indices
                    .extend([corners[a], corners[b], corners[c]]);
            }
        }
    }
}

fn color3(v: Option<&[f64]>, default: [f64; 3]) -> [f64; 3] {
    match v {
        Some([r, g, b, ..]) => [*r, *g, *b],
        _ => default,
    }
}

/// T C R SR S SR⁻¹ C⁻¹.
fn transform_of(node: &Node) -> DMat4 {
    let v3 = |name: &str, default: DVec3| match node.numbers(name) {
        Some([x, y, z, ..]) => DVec3::new(*x, *y, *z),
        _ => default,
    };
    let rot = |name: &str| match node.numbers(name) {
        Some([x, y, z, a, ..]) => {
            let axis = DVec3::new(*x, *y, *z);
            if axis.length_squared() > 0.0 {
                DQuat::from_axis_angle(axis.normalize(), *a)
            } else {
                DQuat::IDENTITY
            }
        }
        _ => DQuat::IDENTITY,
    };
    let translation = v3("translation", DVec3::ZERO);
    let center = v3("center", DVec3::ZERO);
    let scale = v3("scale", DVec3::ONE);
    let rotation = rot("rotation");
    let so = rot("scaleOrientation");
    DMat4::from_translation(translation)
        * DMat4::from_translation(center)
        * DMat4::from_quat(rotation)
        * DMat4::from_quat(so)
        * DMat4::from_scale(scale)
        * DMat4::from_quat(so.inverse())
        * DMat4::from_translation(-center)
}

/// Newell's normal, not normalized.
fn newell(points: impl Iterator<Item = DVec3>) -> DVec3 {
    let p: Vec<DVec3> = points.collect();
    let mut n = DVec3::ZERO;
    for i in 0..p.len() {
        let (a, b) = (p[i], p[(i + 1) % p.len()]);
        n.x += (a.y - b.y) * (a.z + b.z);
        n.y += (a.z - b.z) * (a.x + b.x);
        n.z += (a.x - b.x) * (a.y + b.y);
    }
    n
}

/// Ear clipping, as triples of positions within `face`.
fn triangulate(face: &[usize], points: &[DVec3]) -> Vec<[usize; 3]> {
    let n = face.len();
    if n == 3 {
        return vec![[0, 1, 2]];
    }
    if n > MAX_EAR_CLIP {
        return (1..n - 1).map(|k| [0, k, k + 1]).collect();
    }
    let normal = newell(face.iter().map(|&i| points[i]));
    if normal.length_squared() < 1e-24 {
        return (1..n - 1).map(|k| [0, k, k + 1]).collect();
    }
    let normal = normal.normalize();
    let u = if normal.x.abs() < 0.9 {
        DVec3::X
    } else {
        DVec3::Y
    }
    .cross(normal)
    .normalize();
    let v = normal.cross(u);
    let flat: Vec<(f64, f64)> = face
        .iter()
        .map(|&i| (points[i].dot(u), points[i].dot(v)))
        .collect();
    let cross = |a: (f64, f64), b: (f64, f64), c: (f64, f64)| {
        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
    };
    let mut remaining: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    while remaining.len() > 3 {
        let m = remaining.len();
        let mut clipped = false;
        for k in 0..m {
            let (a, b, c) = (
                remaining[(k + m - 1) % m],
                remaining[k],
                remaining[(k + 1) % m],
            );
            if cross(flat[a], flat[b], flat[c]) <= 1e-18 {
                continue;
            }
            let inside = remaining.iter().any(|&p| {
                p != a
                    && p != b
                    && p != c
                    && cross(flat[a], flat[b], flat[p]) >= 0.0
                    && cross(flat[b], flat[c], flat[p]) >= 0.0
                    && cross(flat[c], flat[a], flat[p]) >= 0.0
            });
            if !inside {
                out.push([a, b, c]);
                remaining.remove(k);
                clipped = true;
                break;
            }
        }
        if !clipped {
            // Degenerate or self-intersecting: fan what is left.
            for k in 1..remaining.len() - 1 {
                out.push([remaining[0], remaining[k], remaining[k + 1]]);
            }
            return out;
        }
    }
    out.push([remaining[0], remaining[1], remaining[2]]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concave_polygons_cover_their_area() {
        let wrl = b"#VRML V2.0 utf8\nShape { geometry IndexedFaceSet { coord Coordinate { point [0 0 0, 2 0 0, 2 1 0, 1 1 0, 1 2 0, 0 2 0] } coordIndex [0 1 2 3 4 5 -1] } }";
        let p = parse(wrl).unwrap();
        let prim = &p.primitives[0].1;
        let mut area = 0.0;
        for t in prim.indices.chunks(3) {
            let v = |i: u32| {
                let p = prim.positions[i as usize];
                DVec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
            };
            area += (v(t[1]) - v(t[0])).cross(v(t[2]) - v(t[0])).length() * 0.5;
        }
        assert!((area - 3.0 * 2.54 * 2.54).abs() < 1e-3, "{area}");
    }

    #[test]
    fn use_chains_that_expand_without_bound_are_refused() {
        // Each level draws the previous one twice: 2^30 shapes from 31 nodes.
        let mut wrl = String::from(
            "#VRML V2.0 utf8\nDEF S0 Shape { geometry IndexedFaceSet { coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0 1 2 -1] } }\n",
        );
        for k in 1..=30 {
            wrl += &format!(
                "DEF S{k} Group {{ children [ USE S{}, USE S{} ] }}\n",
                k - 1,
                k - 1
            );
        }
        let err = parse(wrl.as_bytes()).err().unwrap();
        assert!(err.contains("too complex"), "{err}");
    }
}
