//! Reading a component STEP file and copying its geometry into the output.
//!
//! The donor is never interpreted as geometry. Its `DATA` section is split
//! into statements, the product structure is read to find the displayed
//! representations, and the transitive closure of the geometry they hold
//! is renumbered and copied through verbatim. Assembly nesting is rebuilt
//! with `MAPPED_ITEM`s so a donor with sub-parts keeps its transforms.
//!
//! Every statement is classified once into a [`Kind`] byte; all later
//! passes work on those bytes and on id-indexed tables rather than re-reading
//! entity names.

use std::borrow::Cow;

use memchr::{memchr, memchr2, memchr3};

use crate::Error;
use crate::geom::{Transform, Vec3};
use crate::step::{Context, Placement, Writer};

const NONE: u32 = u32::MAX;
/// KiCad's `USER_PREC`: the precision its OCCT reader applies to models.
const MODEL_ACCURACY: f64 = 1e-4;

#[derive(Clone, Copy)]
struct Stmt {
    id: u32,
    start: u32,
    end: u32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Other,
    /// Complex entity: `( A() B() )`, no single name.
    Complex,
    Geometry,
    SurfaceCurve,
    StyledItem,
    StyleAssignment,
    SurfaceStyleUsage,
    MappedItem,
    ShapeRepresentation,
    ShapeDefinitionRepresentation,
    ProductDefinitionShape,
    AssemblyUsage,
    ContextDependentShapeRepresentation,
    Relationship,
    RelationshipWithTransformation,
    ItemDefinedTransformation,
    Axis2Placement,
    CartesianPoint,
    Direction,
    Unit,
}

fn classify(body: &[u8]) -> Kind {
    let name = entity_kind(body);
    match name {
        b"" => Kind::Complex,
        b"MANIFOLD_SOLID_BREP"
        | b"BREP_WITH_VOIDS"
        | b"FACETED_BREP"
        | b"SHELL_BASED_SURFACE_MODEL"
        | b"GEOMETRIC_SET"
        | b"GEOMETRIC_CURVE_SET" => Kind::Geometry,
        b"SURFACE_CURVE" | b"SEAM_CURVE" | b"BOUNDED_SURFACE_CURVE" => Kind::SurfaceCurve,
        b"STYLED_ITEM" => Kind::StyledItem,
        b"PRESENTATION_STYLE_ASSIGNMENT" => Kind::StyleAssignment,
        b"SURFACE_STYLE_USAGE" => Kind::SurfaceStyleUsage,
        b"MAPPED_ITEM" => Kind::MappedItem,
        b"SHAPE_DEFINITION_REPRESENTATION" => Kind::ShapeDefinitionRepresentation,
        b"PRODUCT_DEFINITION_SHAPE" => Kind::ProductDefinitionShape,
        b"NEXT_ASSEMBLY_USAGE_OCCURRENCE" => Kind::AssemblyUsage,
        b"CONTEXT_DEPENDENT_SHAPE_REPRESENTATION" => Kind::ContextDependentShapeRepresentation,
        b"REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION" => Kind::RelationshipWithTransformation,
        b"ITEM_DEFINED_TRANSFORMATION" => Kind::ItemDefinedTransformation,
        b"AXIS2_PLACEMENT_3D" => Kind::Axis2Placement,
        b"CARTESIAN_POINT" => Kind::CartesianPoint,
        b"DIRECTION" => Kind::Direction,
        _ if name.ends_with(b"_STYLED_ITEM") => Kind::StyledItem,
        _ if name.ends_with(b"SHAPE_REPRESENTATION") => Kind::ShapeRepresentation,
        _ if contains_bytes(name, b"REPRESENTATION_RELATIONSHIP") => Kind::Relationship,
        _ if name.ends_with(b"UNIT") => Kind::Unit,
        _ => Kind::Other,
    }
}

pub(crate) struct Donor {
    data: Vec<u8>,
    stmts: Vec<Stmt>,
    kinds: Vec<Kind>,
    /// Statement index by entity id.
    by_id: Vec<u32>,
    /// Factor from the donor's length unit to millimetres.
    unit_scale: f64,
    /// Factor from the donor's plane angle unit to radians.
    angle_scale: f64,
}

/// Reference lists in one flat array: `items[offsets[i]..offsets[i + 1]]`
/// belongs to statement `i`.
struct Adjacency {
    offsets: Vec<u32>,
    items: Vec<u32>,
}

/// What to copy from a donor and how it is structured, computed before any
/// ids are handed out so that emission can run with a known id budget.
pub(crate) struct Analysis {
    /// Old ids of every entity to copy, ascending.
    closure: Vec<u32>,
    /// Length scale of each entity in `closure`, from the context of the
    /// representation it belongs to.
    scales: Vec<f64>,
    /// Surface curves redirected to their 3D basis curve.
    aliases: Vec<(u32, u32)>,
    reps: Vec<Rep>,
    /// Indices into `reps` of the top-level representations.
    roots: Vec<u32>,
}

struct Rep {
    id: u32,
    /// Old ids of the geometry items directly in this representation.
    geometry: Vec<u32>,
    children: Vec<(u32, Transform)>,
    /// Factor from this representation's length unit to millimetres. A
    /// file can mix contexts, inch parts in a millimetre assembly.
    scale: f64,
}

pub(crate) struct Shape {
    pub(crate) representation: u32,
    pub(crate) origin: u32,
}

impl Donor {
    pub(crate) fn parse(step: Vec<u8>) -> Result<Self, Error> {
        let data_start =
            find_keyword(&step, b"DATA;").ok_or(Error::Step("missing DATA section"))? + 5;
        let data_end = memchr::memmem::rfind(&step[data_start..], b"ENDSEC;")
            .ok_or(Error::Step("missing DATA ENDSEC"))?
            + data_start;
        let data = &step[..data_end];

        let mut stmts = Vec::with_capacity(data.len() / 48);
        let mut max_id = 0u32;
        let mut stmt_start = data_start;
        let mut at = data_start;
        while let Some(off) = memchr2(b'\'', b';', &data[at..]) {
            let hit = at + off;
            if data[hit] == b'\'' {
                at = string_end(data, hit + 1).ok_or(Error::Step("unterminated string"))?;
                continue;
            }
            if let Some(stmt) = parse_statement(data, stmt_start, hit) {
                max_id = max_id.max(stmt.id);
                stmts.push(stmt);
            }
            at = hit + 1;
            stmt_start = at;
        }
        let kinds = stmts
            .iter()
            .map(|s| classify(&step[s.start as usize..s.end as usize]))
            .collect();
        let mut by_id = vec![NONE; max_id as usize + 1];
        for (i, s) in stmts.iter().enumerate() {
            by_id[s.id as usize] = i as u32;
        }
        let mut donor = Donor {
            data: step,
            stmts,
            kinds,
            by_id,
            unit_scale: 1.0,
            angle_scale: 1.0,
        };
        donor.unit_scale = donor.length_unit_scale();
        if donor
            .unit_bodies()
            .any(|body| contains_normalized(body, b"CONVERSION_BASED_UNIT('DEGREE'"))
        {
            donor.angle_scale = std::f64::consts::PI / 180.0;
        }
        Ok(donor)
    }

    fn body(&self, s: Stmt) -> &[u8] {
        &self.data[s.start as usize..s.end as usize]
    }

    fn index(&self, id: u32) -> Option<usize> {
        let i = *self.by_id.get(id as usize)?;
        (i != NONE).then_some(i as usize)
    }

    fn get(&self, id: u32) -> Option<Stmt> {
        self.index(id).map(|i| self.stmts[i])
    }

    fn kind(&self, id: u32) -> Kind {
        self.index(id).map_or(Kind::Other, |i| self.kinds[i])
    }

    fn refs(&self, id: u32) -> Vec<u32> {
        let mut out = Vec::new();
        if let Some(s) = self.get(id) {
            scan_refs(self.body(s), |r| out.push(r));
        }
        out
    }

    fn of_kind(&self, kind: Kind) -> impl Iterator<Item = Stmt> + '_ {
        self.stmts
            .iter()
            .zip(&self.kinds)
            .filter(move |(_, k)| **k == kind)
            .map(|(s, _)| *s)
    }

    /// Statements that can declare a unit: named `*_UNIT` entities and
    /// complex entities, which have no single name.
    fn unit_bodies(&self) -> impl Iterator<Item = &[u8]> + Clone {
        self.stmts
            .iter()
            .zip(&self.kinds)
            .filter(|(_, k)| matches!(k, Kind::Unit | Kind::Complex))
            .map(|(s, _)| self.body(*s))
    }

    /// The length scale declared by the context of representation `rep`:
    /// its last argument names the context, whose unit assignment names
    /// the units.
    fn context_scale(&self, rep: u32) -> Option<f64> {
        let stmt = self.get(rep)?;
        let body = self.body(stmt);
        let mut context = None;
        scan_refs(&body[arg_range(body, 2)?], |r| context = Some(r));
        let context = self.get(context?)?;
        let mut units = Vec::new();
        scan_refs(self.body(context), |r| units.push(r));
        units.iter().find_map(|u| {
            let body = self.body(self.get(*u)?);
            contains_normalized(body, b"LENGTH_UNIT").then(|| unit_body_scale(body))?
        })
    }

    fn length_unit_scale(&self) -> f64 {
        const SCALES: [(&[u8], f64); 4] = [
            (b"SI_UNIT(.MILLI.,.METRE.)", 1.0),
            (b"SI_UNIT($,.METRE.)", 1000.0),
            (b"SI_UNIT(.CENTI.,.METRE.)", 10.0),
            (b"SI_UNIT(.MICRO.,.METRE.)", 0.001),
        ];
        for body in self.unit_bodies() {
            if contains_normalized(body, b"CONVERSION_BASED_UNIT('INCH'") {
                return 25.4;
            }
        }
        for body in self.unit_bodies() {
            if !contains_normalized(body, b"LENGTH_UNIT") {
                continue;
            }
            for (pattern, scale) in SCALES {
                if contains_normalized(body, pattern) {
                    return scale;
                }
            }
        }
        1.0
    }
}

/// The scale to millimetres a length unit statement declares, if it is
/// one this exporter knows.
fn unit_body_scale(body: &[u8]) -> Option<f64> {
    const SCALES: [(&[u8], f64); 5] = [
        (b"CONVERSION_BASED_UNIT('INCH'", 25.4),
        (b"SI_UNIT(.MILLI.,.METRE.)", 1.0),
        (b"SI_UNIT($,.METRE.)", 1000.0),
        (b"SI_UNIT(.CENTI.,.METRE.)", 10.0),
        (b"SI_UNIT(.MICRO.,.METRE.)", 0.001),
    ];
    SCALES
        .iter()
        .find(|(pattern, _)| contains_normalized(body, pattern))
        .map(|(_, scale)| *scale)
}

impl Donor {
    pub(crate) fn analyze(&self) -> Result<Analysis, Error> {
        let mut roots = self.displayed_roots();
        roots.sort_unstable();
        roots.dedup();

        if roots.is_empty() {
            let mut geometry: Vec<u32> = self.of_kind(Kind::Geometry).map(|s| s.id).collect();
            geometry.sort_unstable();
            let (closure, aliases) = self.closure(&geometry);
            let scales = vec![self.unit_scale; closure.len()];
            return Ok(Analysis {
                closure,
                scales,
                aliases,
                reps: vec![Rep {
                    id: 0,
                    geometry,
                    children: Vec::new(),
                    scale: self.unit_scale,
                }],
                roots: vec![0],
            });
        }

        let mut reps: Vec<Rep> = Vec::new();
        let mut rep_index = |id: u32, reps: &mut Vec<Rep>| -> u32 {
            if let Some(i) = reps.iter().position(|r| r.id == id) {
                return i as u32;
            }
            reps.push(Rep {
                id,
                geometry: Vec::new(),
                children: Vec::new(),
                scale: self.context_scale(id).unwrap_or(self.unit_scale),
            });
            (reps.len() - 1) as u32
        };
        let structure = self.product_structure()?;
        let mut path = Vec::new();
        for root in &roots {
            let index = rep_index(*root, &mut reps);
            self.collect_tree(index, &structure, &mut reps, &mut rep_index, &mut path);
        }
        for rep in &mut reps {
            rep.children
                .sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp_bits(&b.1)));
        }

        let mut all_geometry = Vec::new();
        for rep in &mut reps {
            let Some(stmt) = self.get(rep.id) else {
                continue;
            };
            let mut items = Vec::new();
            if let Some(range) = arg_range(self.body(stmt), 1) {
                scan_refs(&self.body(stmt)[range], |r| items.push(r));
            }
            if items.iter().any(|i| self.kind(*i) == Kind::MappedItem) {
                return Err(Error::Step(
                    "donor uses MAPPED_ITEM, which is not supported",
                ));
            }
            items.retain(|i| self.kind(*i) == Kind::Geometry);
            items.sort_unstable();
            items.dedup();
            all_geometry.extend_from_slice(&items);
            rep.geometry = items;
        }
        let (closure, aliases) = self.closure(&all_geometry);
        // Each entity takes the scale of the first representation that
        // reaches it.
        let mut scale_of = vec![0.0f64; self.by_id.len()];
        for rep in &reps {
            if rep.scale == self.unit_scale {
                continue;
            }
            let (own, own_aliases) = self.closure(&rep.geometry);
            for id in own.iter().chain(own_aliases.iter().map(|(from, _)| from)) {
                if scale_of[*id as usize] == 0.0 {
                    scale_of[*id as usize] = rep.scale;
                }
            }
        }
        let scales = closure
            .iter()
            .map(|id| {
                let s = scale_of[*id as usize];
                if s == 0.0 { self.unit_scale } else { s }
            })
            .collect();
        let root_indices = roots
            .iter()
            .map(|r| reps.iter().position(|rep| rep.id == *r).unwrap() as u32)
            .collect();
        Ok(Analysis {
            closure,
            scales,
            aliases,
            reps,
            roots: root_indices,
        })
    }

    fn collect_tree(
        &self,
        rep: u32,
        structure: &Structure,
        reps: &mut Vec<Rep>,
        rep_index: &mut impl FnMut(u32, &mut Vec<Rep>) -> u32,
        path: &mut Vec<u32>,
    ) {
        let id = reps[rep as usize].id;
        if path.contains(&id) {
            return;
        }
        path.push(id);
        let first = structure.children.partition_point(|e| e.0 < id);
        let mut children = Vec::new();
        for edge in &structure.children[first..] {
            if edge.0 != id {
                break;
            }
            if !path.contains(&edge.1) {
                children.push((edge.1, edge.2));
            }
        }
        for (child_id, transform) in children {
            let known = reps.iter().any(|r| r.id == child_id);
            let child = rep_index(child_id, reps);
            reps[rep as usize].children.push((child, transform));
            if !known {
                self.collect_tree(child, structure, reps, rep_index, path);
            }
        }
        path.pop();
    }

    /// Parent-to-child representation edges, sorted by parent.
    ///
    /// Assembly children come from `NEXT_ASSEMBLY_USAGE_OCCURRENCE`, whose
    /// `CONTEXT_DEPENDENT_SHAPE_REPRESENTATION` names the representation
    /// relationship carrying the child's placement. That is the direction
    /// OCCT follows; the representation relationships alone do not say
    /// which side is the parent. Relationships with no occurrence link a
    /// product's own representations and are followed both ways at identity.
    fn product_structure(&self) -> Result<Structure, Error> {
        // representation -> product definition, via SHAPE_DEFINITION_REPRESENTATION
        let mut rep_product: Vec<(u32, u32)> = Vec::new();
        for s in self.of_kind(Kind::ShapeDefinitionRepresentation) {
            let refs = self.refs(s.id);
            let (Some(&shape_def), Some(&rep)) = (refs.first(), refs.get(1)) else {
                continue;
            };
            if let Some(product) = self.refs(shape_def).last().copied() {
                rep_product.push((rep, product));
            }
        }
        rep_product.sort_unstable();
        let product_of = |rep: u32| -> Option<u32> {
            rep_product
                .binary_search_by_key(&rep, |e| e.0)
                .ok()
                .map(|i| rep_product[i].1)
        };
        // NEXT_ASSEMBLY_USAGE_OCCURRENCE -> (parent product, child product)
        let mut usages: Vec<(u32, u32, u32)> = Vec::new();
        for s in self.of_kind(Kind::AssemblyUsage) {
            let refs = self.refs(s.id);
            if let (Some(&parent), Some(&child)) = (refs.first(), refs.get(1)) {
                usages.push((s.id, parent, child));
            }
        }
        usages.sort_unstable();
        // relationship -> usage, via CONTEXT_DEPENDENT_SHAPE_REPRESENTATION
        // and its PRODUCT_DEFINITION_SHAPE
        let mut relationship_usage: Vec<(u32, u32)> = Vec::new();
        for s in self.of_kind(Kind::ContextDependentShapeRepresentation) {
            let refs = self.refs(s.id);
            let (Some(&relationship), Some(&shape_def)) = (refs.first(), refs.get(1)) else {
                continue;
            };
            if let Some(usage) = self.refs(shape_def).last().copied()
                && usages.binary_search_by_key(&usage, |u| u.0).is_ok()
            {
                relationship_usage.push((relationship, usage));
            }
        }
        relationship_usage.sort_unstable();

        let mut children: Vec<(u32, u32, Transform)> = Vec::new();
        for (s, kind) in self.stmts.iter().zip(&self.kinds) {
            let body = self.body(*s);
            let transformed = match kind {
                Kind::Relationship => false,
                Kind::RelationshipWithTransformation => true,
                Kind::Complex => {
                    if find_keyword(body, b"REPRESENTATION_RELATIONSHIP").is_none() {
                        continue;
                    }
                    find_keyword(body, b"REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION").is_some()
                }
                _ => continue,
            };
            let mut refs = Vec::new();
            scan_refs(body, |r| refs.push(r));
            let (Some(&first), Some(&second)) = (refs.first(), refs.get(1)) else {
                continue;
            };
            let usage = relationship_usage
                .binary_search_by_key(&s.id, |e| e.0)
                .ok()
                .map(|i| relationship_usage[i].1);
            if !transformed {
                if usage.is_none() {
                    children.push((first, second, Transform::IDENTITY));
                    children.push((second, first, Transform::IDENTITY));
                }
                continue;
            }
            let transformation = refs.get(2).copied().ok_or(Error::Step(
                "transformed relationship without transformation",
            ))?;
            let (a, b) = self
                .item_defined_transformation(transformation)
                .ok_or(Error::Step("unsupported representation transformation"))?;
            // The child is whichever side belongs to the occurrence's child
            // product; without an occurrence, the first side by convention.
            let first_is_child = match usage {
                Some(usage) => {
                    let child_product =
                        usages[usages.binary_search_by_key(&usage, |u| u.0).unwrap()].2;
                    product_of(first) == Some(child_product)
                        || product_of(second) != Some(child_product)
                }
                None => true,
            };
            if first_is_child {
                children.push((second, first, b.then(&a.inverse())));
            } else {
                children.push((first, second, a.then(&b.inverse())));
            }
        }
        children.sort_by(|x, y| x.0.cmp(&y.0).then(x.1.cmp(&y.1)));
        Ok(Structure { children })
    }

    /// Representations of products that are not used as a child of another
    /// product, i.e. what a viewer would show.
    fn displayed_roots(&self) -> Vec<u32> {
        let mut child_products = Vec::new();
        for s in self.of_kind(Kind::AssemblyUsage) {
            let mut refs = Vec::new();
            scan_refs(self.body(s), |r| refs.push(r));
            if let Some(child) = refs.get(1) {
                child_products.push(*child);
            }
        }
        child_products.sort_unstable();
        let mut roots = Vec::new();
        for s in self.of_kind(Kind::ShapeDefinitionRepresentation) {
            let mut refs = Vec::new();
            scan_refs(self.body(s), |r| refs.push(r));
            let (Some(&shape_def), Some(&rep)) = (refs.first(), refs.get(1)) else {
                continue;
            };
            if self.kind(shape_def) != Kind::ProductDefinitionShape {
                continue;
            }
            let Some(product_def) = self.refs(shape_def).last().copied() else {
                continue;
            };
            if child_products.binary_search(&product_def).is_ok() {
                continue;
            }
            roots.push(rep);
        }
        if roots.is_empty() {
            roots.extend(self.of_kind(Kind::ShapeRepresentation).map(|s| s.id));
        }
        roots
    }

    fn item_defined_transformation(&self, id: u32) -> Option<(Transform, Transform)> {
        if self.kind(id) != Kind::ItemDefinedTransformation {
            return None;
        }
        let refs = self.refs(id);
        Some((
            self.axis2_placement(*refs.first()?)?,
            self.axis2_placement(*refs.get(1)?)?,
        ))
    }

    fn axis2_placement(&self, id: u32) -> Option<Transform> {
        if self.kind(id) != Kind::Axis2Placement {
            return None;
        }
        let refs = self.refs(id);
        let origin = self.vec3(*refs.first()?, Kind::CartesianPoint)?;
        let z = self.vec3(*refs.get(1)?, Kind::Direction)?;
        let x = self.vec3(*refs.get(2)?, Kind::Direction)?;
        Some(Transform::from_axes(origin, x, z))
    }

    fn vec3(&self, id: u32, kind: Kind) -> Option<Vec3> {
        if self.kind(id) != kind {
            return None;
        }
        last_vec3_tuple(self.body(self.get(id)?)).map(|t| t.2)
    }

    /// Ascending ids of everything reachable from `roots`, plus the styled
    /// items that colour them.
    ///
    /// Surface curves are not copied: like OCCT's writer in its default
    /// mode, references to them are redirected to the 3D basis curve, which
    /// drops every parametric curve and its 2D geometry from the output.
    fn closure(&self, roots: &[u32]) -> (Vec<u32>, Vec<(u32, u32)>) {
        let styles = self.surface_style_index();
        let refs = self.references();
        let mut seen = vec![false; self.by_id.len()];
        let mut stack: Vec<u32> = Vec::new();
        let mut ids = Vec::new();
        let mut aliases: Vec<(u32, u32)> = Vec::new();
        let mut push = |mut id: u32, stack: &mut Vec<u32>, ids: &mut Vec<u32>| {
            if let Some(basis) = self.surface_curve_basis(id) {
                aliases.push((id, basis));
                id = basis;
            }
            if let Some(s) = seen.get_mut(id as usize)
                && !*s
            {
                *s = true;
                stack.push(id);
                ids.push(id);
            }
        };
        for r in roots {
            push(*r, &mut stack, &mut ids);
        }
        while let Some(id) = stack.pop() {
            if let Some(index) = self.index(id) {
                let (lo, hi) = (refs.offsets[index], refs.offsets[index + 1]);
                for r in &refs.items[lo as usize..hi as usize] {
                    push(*r, &mut stack, &mut ids);
                }
            }
            let (lo, hi) = (styles.offsets[id as usize], styles.offsets[id as usize + 1]);
            for styled in &styles.items[lo as usize..hi as usize] {
                push(*styled, &mut stack, &mut ids);
            }
        }
        ids.sort_unstable();
        aliases.sort_unstable();
        aliases.dedup();
        (ids, aliases)
    }

    /// The entity references of every statement, by statement index.
    /// Scanning the text is most of the closure's cost, and a large
    /// donor is scanned on several threads.
    fn references(&self) -> Adjacency {
        let chunk = (self.stmts.len() / self.threads()).max(1);
        let ranges: Vec<std::ops::Range<usize>> = (0..self.stmts.len())
            .step_by(chunk)
            .map(|start| start..(start + chunk).min(self.stmts.len()))
            .collect();
        let scanned = crate::parallel_map(ranges.len(), &ranges, |range| {
            let mut offsets = Vec::with_capacity(range.len());
            let mut items = Vec::new();
            for stmt in &self.stmts[range.clone()] {
                offsets.push(items.len() as u32);
                scan_refs(self.body(*stmt), |r| items.push(r));
            }
            (offsets, items)
        });
        let mut offsets = Vec::with_capacity(self.stmts.len() + 1);
        let mut items = Vec::new();
        for (chunk_offsets, chunk_items) in scanned {
            let base = items.len() as u32;
            offsets.extend(chunk_offsets.iter().map(|o| o + base));
            items.extend(chunk_items);
        }
        offsets.push(items.len() as u32);
        Adjacency { offsets, items }
    }

    /// Worker threads worth spending on this donor: only a large one is
    /// worth splitting, since the models of a board are already copied
    /// in parallel.
    fn threads(&self) -> usize {
        if self.data.len() < 4 << 20 {
            1
        } else {
            crate::worker_threads()
        }
    }

    /// The 3D basis curve of a surface curve, or of a complex entity that
    /// contains one.
    fn surface_curve_basis(&self, id: u32) -> Option<u32> {
        let index = self.index(id)?;
        let body = self.body(self.stmts[index]);
        let at = match self.kinds[index] {
            Kind::SurfaceCurve => 0,
            Kind::Complex => find_keyword(body, b"SURFACE_CURVE(")?,
            _ => return None,
        };
        let mut basis = None;
        scan_refs(&body[at..], |r| {
            basis.get_or_insert(r);
        });
        basis
    }

    /// Styled items with a surface style, grouped by the item they style.
    /// Curve and point styles are left behind: they colour edges, which
    /// nothing downstream of a solid model shows.
    fn surface_style_index(&self) -> StyleIndex {
        let mut pairs: Vec<(u32, u32)> = Vec::new();
        for s in self.of_kind(Kind::StyledItem) {
            let body = self.body(s);
            let Some(target_range) = arg_range(body, 2) else {
                continue;
            };
            let mut target = None;
            scan_refs(&body[target_range], |r| {
                target.get_or_insert(r);
            });
            let Some(target) = target else {
                continue;
            };
            let Some(style_range) = arg_range(body, 1) else {
                continue;
            };
            let mut has_surface = false;
            scan_refs(&body[style_range], |assignment| {
                for r in self.refs(assignment) {
                    if self.kind(r) == Kind::SurfaceStyleUsage {
                        has_surface = true;
                    }
                }
            });
            if has_surface && (target as usize) < self.by_id.len() {
                pairs.push((target, s.id));
            }
        }
        pairs.sort_unstable();
        let mut offsets = vec![0u32; self.by_id.len() + 1];
        for (target, _) in &pairs {
            offsets[*target as usize + 1] += 1;
        }
        for i in 1..offsets.len() {
            offsets[i] += offsets[i - 1];
        }
        StyleIndex {
            offsets,
            items: pairs.into_iter().map(|(_, id)| id).collect(),
        }
    }
}

struct StyleIndex {
    offsets: Vec<u32>,
    items: Vec<u32>,
}

struct Structure {
    /// `(parent representation, child representation, child placement)`.
    children: Vec<(u32, u32, Transform)>,
}

impl Analysis {
    pub(crate) fn is_empty(&self) -> bool {
        self.reps.iter().all(|r| r.geometry.is_empty())
    }

    /// Exactly how many ids `emit` consumes.
    pub(crate) fn id_budget(&self) -> u32 {
        let reps: usize = self.reps.iter().map(|r| 6 + 5 * r.children.len()).sum();
        let wrapper = if self.roots.len() == 1 {
            0
        } else {
            5 + 5 * self.roots.len()
        };
        (self.closure.len() + reps + wrapper + 2) as u32
    }

    /// Copy the closure and rebuild the representation tree. `scale` is
    /// baked into the copied coordinates along with the donor's unit.
    ///
    /// The copied representations get their own context declaring the
    /// distance accuracy KiCad reads models at, since readers sew geometry
    /// at the declared accuracy and donors are often sloppier than the
    /// 1 µm of the board context.
    pub(crate) fn emit(
        &self,
        donor: &Donor,
        w: &mut Writer,
        context: &Context,
        scale: f64,
    ) -> Result<Shape, Error> {
        let geom_context = w.geometry_context(context, MODEL_ACCURACY);
        let base = w.next_id;
        w.next_id += self.closure.len() as u32;
        let mut map = vec![0u32; donor.by_id.len()];
        for (rank, old) in self.closure.iter().enumerate() {
            map[*old as usize] = base + rank as u32;
        }
        for (from, to) in &self.aliases {
            map[*from as usize] = map[*to as usize];
        }
        let new_id = |old: u32| -> Option<u32> {
            match map.get(old as usize) {
                Some(&id) if id != 0 => Some(id),
                _ => None,
            }
        };

        // Statements are copied in rank order; a large donor is copied
        // in chunks on several threads and the chunks joined.
        let copy_range = |range: std::ops::Range<usize>| -> Result<Vec<u8>, Error> {
            let mut out = Vec::with_capacity(range.len() * 72);
            for rank in range {
                let Some(stmt) = donor.get(self.closure[rank]) else {
                    continue;
                };
                let coordinate_scale = self.scales[rank] * scale;
                let transform = (coordinate_scale != 1.0)
                    .then(|| Transform(glam::DMat4::from_scale(Vec3::splat(coordinate_scale))));
                let body = donor.body(stmt);
                let body = if transform.is_some() || donor.angle_scale != 1.0 {
                    convert_body(
                        body,
                        transform.as_ref(),
                        coordinate_scale,
                        donor.angle_scale,
                    )?
                } else {
                    Cow::Borrowed(body)
                };
                out.push(b'#');
                crate::step::push_uint(&mut out, base + rank as u32);
                out.extend_from_slice(b" = ");
                copy_body(&body, &new_id, &mut out);
                out.extend_from_slice(b";\n");
            }
            Ok(out)
        };
        let threads = donor.threads();
        let chunk = self.closure.len().div_ceil(threads).max(1);
        let ranges: Vec<std::ops::Range<usize>> = (0..self.closure.len())
            .step_by(chunk)
            .map(|start| start..(start + chunk).min(self.closure.len()))
            .collect();
        for copied in crate::parallel_map(threads, &ranges, |r| copy_range(r.clone())) {
            w.buf.extend_from_slice(&copied?);
        }

        // Every representation reserves its ids first so children can be
        // referenced before they are written.
        struct Ids {
            placement: Placement,
            representation: u32,
            map: u32,
        }
        let ids: Vec<Ids> = self
            .reps
            .iter()
            .map(|_| Ids {
                placement: Placement::reserve(w),
                representation: w.id(),
                map: w.id(),
            })
            .collect();
        let mut items: Vec<u32> = Vec::new();
        for (index, rep) in self.reps.iter().enumerate() {
            let own = &ids[index];
            w.axis_placement_at(own.placement, &Transform::IDENTITY);
            items.clear();
            items.push(own.placement.axis);
            items.extend(rep.geometry.iter().filter_map(|g| new_id(*g)));
            for (child, transform) in &rep.children {
                let placement = Placement::reserve(w);
                w.axis_placement_at(placement, &transform.scale_translation(rep.scale * scale));
                let mapped = w.mapped_item(ids[*child as usize].map, placement.axis);
                items.push(placement.axis);
                items.push(mapped);
            }
            w.shape_representation(own.representation, &items, geom_context);
            w.representation_map_at(own.map, own.placement.axis, own.representation);
        }

        if let [root] = self.roots.as_slice() {
            let root = &ids[*root as usize];
            return Ok(Shape {
                representation: root.representation,
                origin: root.placement.axis,
            });
        }
        let placement = Placement::reserve(w);
        w.axis_placement_at(placement, &Transform::IDENTITY);
        items.clear();
        items.push(placement.axis);
        for root in &self.roots {
            let child = Placement::reserve(w);
            w.axis_placement_at(child, &Transform::IDENTITY);
            let mapped = w.mapped_item(ids[*root as usize].map, child.axis);
            items.push(child.axis);
            items.push(mapped);
        }
        let representation = w.id();
        w.shape_representation(representation, &items, geom_context);
        Ok(Shape {
            representation,
            origin: placement.axis,
        })
    }
}

fn parse_statement(data: &[u8], start: usize, end: usize) -> Option<Stmt> {
    let mut i = start;
    while i < end && data[i].is_ascii_whitespace() {
        i += 1;
    }
    if data.get(i) != Some(&b'#') {
        return None;
    }
    let (id, after) = parse_uint(data, i + 1)?;
    let eq = memchr(b'=', &data[after..end])? + after;
    let mut body_start = eq + 1;
    while body_start < end && data[body_start].is_ascii_whitespace() {
        body_start += 1;
    }
    let mut body_end = end;
    while body_end > body_start && data[body_end - 1].is_ascii_whitespace() {
        body_end -= 1;
    }
    Some(Stmt {
        id: u32::try_from(id).ok()?,
        start: body_start as u32,
        end: body_end as u32,
    })
}

/// Leading entity name; empty for a complex `( A() B() )` entity.
fn entity_kind(body: &[u8]) -> &[u8] {
    let end = body
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || *b == b'_'))
        .unwrap_or(body.len());
    &body[..end]
}

/// Index just past the closing quote of a string opened before `at`.
fn string_end(data: &[u8], mut at: usize) -> Option<usize> {
    while let Some(off) = memchr(b'\'', &data[at..]) {
        at += off + 1;
        if data.get(at) == Some(&b'\'') {
            at += 1;
        } else {
            return Some(at);
        }
    }
    None
}

fn parse_uint(data: &[u8], start: usize) -> Option<(u64, usize)> {
    let mut i = start;
    let mut value = 0u64;
    while let Some(&b) = data.get(i) {
        if !b.is_ascii_digit() {
            break;
        }
        value = value.checked_mul(10)?.checked_add((b - b'0') as u64)?;
        i += 1;
    }
    (i > start).then_some((value, i))
}

/// Visit every `#id` reference outside strings.
fn scan_refs(body: &[u8], mut visit: impl FnMut(u32)) {
    let mut at = 0;
    while let Some(off) = memchr2(b'\'', b'#', &body[at..]) {
        at += off;
        if body[at] == b'\'' {
            at = string_end(body, at + 1).unwrap_or(body.len());
        } else if let Some((id, next)) = parse_uint(body, at + 1) {
            if let Ok(id) = u32::try_from(id) {
                visit(id);
            }
            at = next;
        } else {
            at += 1;
        }
    }
}

/// Copy `body` to `out` with each `#id` mapped through `new_id` and each
/// real literal rewritten to 12 significant digits, the precision OCCT
/// writes, which is what donor files carry too much of.
fn copy_body(body: &[u8], new_id: &dyn Fn(u32) -> Option<u32>, out: &mut Vec<u8>) {
    let mut at = 0;
    let mut copied = 0;
    while let Some(off) = memchr3(b'\'', b'#', b'.', &body[at..]) {
        at += off;
        match body[at] {
            b'\'' => at = string_end(body, at + 1).unwrap_or(body.len()),
            b'#' => {
                if let Some((id, next)) = parse_uint(body, at + 1)
                    && let Some(id) = u32::try_from(id).ok().and_then(new_id)
                {
                    out.extend_from_slice(&body[copied..at]);
                    out.push(b'#');
                    crate::step::push_uint(out, id);
                    at = next;
                    copied = at;
                } else {
                    at += 1;
                }
            }
            _ => {
                // A real has a digit before its point; enumerations do not.
                if at == 0 || !body[at - 1].is_ascii_digit() {
                    at += 1;
                    continue;
                }
                let mut start = at - 1;
                while start > 0 && body[start - 1].is_ascii_digit() {
                    start -= 1;
                }
                if start > 0 && (body[start - 1] == b'-' || body[start - 1] == b'+') {
                    start -= 1;
                }
                let mut end = at + 1;
                while end < body.len() && body[end].is_ascii_digit() {
                    end += 1;
                }
                if end < body.len() && (body[end] == b'E' || body[end] == b'e') {
                    let mut e = end + 1;
                    if e < body.len() && (body[e] == b'-' || body[e] == b'+') {
                        e += 1;
                    }
                    let digits = e;
                    while e < body.len() && body[e].is_ascii_digit() {
                        e += 1;
                    }
                    if e > digits {
                        end = e;
                    }
                }
                if let Some(value) = parse_f64(&body[start..end]) {
                    out.extend_from_slice(&body[copied..start]);
                    push_real(out, value);
                    copied = end;
                }
                at = end;
            }
        }
    }
    out.extend_from_slice(&body[copied..]);
}

const POW10: [f64; 23] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22,
];

fn pow10(k: i32) -> f64 {
    if (0..POW10.len() as i32).contains(&k) {
        POW10[k as usize]
    } else if (-(POW10.len() as i32) + 1..0).contains(&k) {
        1.0 / POW10[(-k) as usize]
    } else {
        10f64.powi(k)
    }
}

/// Real literal with 12 significant digits in OCCT's style: fixed notation
/// down to 0.1, exponent notation below that, trailing zeros trimmed.
pub(crate) fn push_real(out: &mut Vec<u8>, value: f64) {
    if value == 0.0 || !value.is_finite() {
        out.extend_from_slice(b"0.");
        return;
    }
    if value < 0.0 {
        out.push(b'-');
    }
    let a = value.abs();
    let mut exp = a.log10().floor() as i32;
    let mut n = (a * pow10(11 - exp)).round() as u64;
    if n >= 1_000_000_000_000 {
        n /= 10;
        exp += 1;
    } else if n < 100_000_000_000 {
        n *= 10;
        exp -= 1;
    }
    let mut digits = [b'0'; 12];
    for slot in digits.iter_mut().rev() {
        *slot = b'0' + (n % 10) as u8;
        n /= 10;
    }
    let mut len = 12;
    while len > 1 && digits[len - 1] == b'0' {
        len -= 1;
    }
    if (-1..=11).contains(&exp) {
        if exp < 0 {
            out.extend_from_slice(b"0.");
            out.extend_from_slice(&digits[..len]);
        } else {
            let int_len = (exp + 1) as usize;
            if len <= int_len {
                out.extend_from_slice(&digits[..len]);
                out.extend(std::iter::repeat_n(b'0', int_len - len));
                out.push(b'.');
            } else {
                out.extend_from_slice(&digits[..int_len]);
                out.push(b'.');
                out.extend_from_slice(&digits[int_len..len]);
            }
        }
    } else {
        out.push(digits[0]);
        out.push(b'.');
        out.extend_from_slice(&digits[1..len]);
        out.push(b'E');
        out.push(if exp < 0 { b'-' } else { b'+' });
        let e = exp.unsigned_abs();
        if e < 10 {
            out.push(b'0');
        }
        crate::step::push_uint(out, e);
    }
}

/// Case-insensitive search anchored on the first byte with memchr.
fn find_keyword(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    let (&first, rest) = needle.split_first()?;
    let (upper, lower) = (first.to_ascii_uppercase(), first.to_ascii_lowercase());
    let mut at = 0;
    while let Some(off) = memchr2(upper, lower, &haystack[at..]) {
        let start = at + off;
        if haystack[start + 1..]
            .get(..rest.len())
            .is_some_and(|w| w.eq_ignore_ascii_case(rest))
        {
            return Some(start);
        }
        at = start + 1;
    }
    None
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Case-insensitive search ignoring whitespace in the haystack.
fn contains_normalized(haystack: &[u8], needle: &[u8]) -> bool {
    'outer: for start in 0..haystack.len() {
        if haystack[start].is_ascii_whitespace()
            || !haystack[start].eq_ignore_ascii_case(&needle[0])
        {
            continue;
        }
        let mut i = start;
        for expected in needle {
            while i < haystack.len() && haystack[i].is_ascii_whitespace() {
                i += 1;
            }
            if i == haystack.len() || !haystack[i].eq_ignore_ascii_case(expected) {
                continue 'outer;
            }
            i += 1;
        }
        return true;
    }
    false
}

/// Byte range of the `index`th top-level argument of an entity.
fn arg_range(body: &[u8], index: usize) -> Option<std::ops::Range<usize>> {
    let open = memchr(b'(', body)?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut arg = 0usize;
    let mut arg_start = open + 1;
    let mut i = open + 1;
    while i < body.len() {
        let b = body[i];
        if b == b'\'' {
            if in_string && body.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            in_string = !in_string;
        } else if !in_string {
            match b {
                b'(' => depth += 1,
                b')' if depth == 0 => {
                    return (arg == index).then_some(arg_start..i);
                }
                b')' => depth -= 1,
                b',' if depth == 0 => {
                    if arg == index {
                        return Some(arg_start..i);
                    }
                    arg += 1;
                    arg_start = i + 1;
                }
                _ => {}
            }
        }
        i += 1;
    }
    None
}

fn parse_f64(bytes: &[u8]) -> Option<f64> {
    std::str::from_utf8(bytes).ok()?.trim().parse().ok()
}

/// The last `(x,y,z)` tuple of a body, with its byte range.
fn last_vec3_tuple(body: &[u8]) -> Option<(usize, usize, Vec3)> {
    let outer_end = body.iter().rposition(|b| *b == b')')?;
    let start = body[..outer_end].iter().rposition(|b| *b == b'(')?;
    let end = memchr(b')', &body[start..])? + start;
    let inner = &body[start + 1..end];
    let mut parts = inner.split(|b| *b == b',');
    let x = parse_f64(parts.next()?)?;
    let y = parse_f64(parts.next()?)?;
    let z = parse_f64(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some((start, end + 1, Vec3::new(x, y, z)))
}

/// Apply a transform to a point or direction entity, and convert the
/// length and angle parameters of curved geometry to millimetres and
/// radians.
fn convert_body<'b>(
    body: &'b [u8],
    transform: Option<&Transform>,
    length_scale: f64,
    angle_scale: f64,
) -> Result<Cow<'b, [u8]>, Error> {
    let kind = entity_kind(body);
    if kind == b"CARTESIAN_POINT" || kind == b"DIRECTION" {
        let (Some(transform), Some((start, end, v))) = (transform, last_vec3_tuple(body)) else {
            return Ok(Cow::Borrowed(body));
        };
        let v = if kind == b"DIRECTION" {
            transform.direction(v)
        } else {
            transform.point(v)
        };
        let mut text = Vec::with_capacity(body.len() + 32);
        text.extend_from_slice(&body[..start]);
        text.push(b'(');
        push_real(&mut text, v.x);
        text.push(b',');
        push_real(&mut text, v.y);
        text.push(b',');
        push_real(&mut text, v.z);
        text.push(b')');
        text.extend_from_slice(&body[end..]);
        return Ok(Cow::Owned(text));
    }
    let scaled: &[(usize, f64)] = match kind {
        b"VECTOR" | b"CIRCLE" | b"CYLINDRICAL_SURFACE" | b"SPHERICAL_SURFACE" => {
            &[(2, length_scale)]
        }
        b"CONICAL_SURFACE" => &[(2, length_scale), (3, angle_scale)],
        b"TOROIDAL_SURFACE" | b"DEGENERATE_TOROIDAL_SURFACE" | b"ELLIPSE" => {
            &[(2, length_scale), (3, length_scale)]
        }
        _ => return Ok(Cow::Borrowed(body)),
    };
    if scaled.iter().all(|(_, factor)| *factor == 1.0) {
        return Ok(Cow::Borrowed(body));
    }
    let mut text = body.to_vec();
    for (index, factor) in scaled.iter().rev() {
        let Some(range) = arg_range(body, *index) else {
            continue;
        };
        let value = parse_f64(&body[range.clone()]).ok_or(Error::Step("bad length parameter"))?;
        let mut formatted = Vec::new();
        push_real(&mut formatted, value * factor);
        text.splice(range, formatted);
    }
    Ok(Cow::Owned(text))
}
