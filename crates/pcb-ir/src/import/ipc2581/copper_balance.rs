//! The `diode.*` NonstandardAttribute metadata generated copper-balance sets
//! carry, and the validation of the structure it promises.

use super::*;

pub const COPPER_BALANCE_ATTRIBUTE_NAME: &str = "diode.copper_balance";
pub const COPPER_BALANCE_LATTICE_ATTRIBUTE_NAME: &str = "diode.copper_balance_lattice";
pub const COPPER_BALANCE_LATTICE_ORIGIN_X_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_origin_x_mm";
pub const COPPER_BALANCE_LATTICE_ORIGIN_Y_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_origin_y_mm";
pub const COPPER_BALANCE_LATTICE_PITCH_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_lattice_pitch_mm";
pub const COPPER_BALANCE_VOID_RADIUS_ATTRIBUTE_NAME: &str = "diode.copper_balance_void_radius_mm";
pub const COPPER_BALANCE_VOID_CORNER_RADIUS_ATTRIBUTE_NAME: &str =
    "diode.copper_balance_void_corner_radius_mm";
pub const COPPER_BALANCE_LATTICE_VALUE: &str = "staggered-hex-v1";

/// The role a generated copper-balance feature set declares through
/// [`COPPER_BALANCE_ATTRIBUTE_NAME`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopperBalanceKind {
    Plane,
    FullVoid,
    /// A boundary void emitted as an explicit clipped contour rather than a
    /// lattice hex instance.
    EdgeVoid,
    BoundaryWeb,
}

impl CopperBalanceKind {
    const ALL: [Self; 4] = [
        Self::Plane,
        Self::FullVoid,
        Self::EdgeVoid,
        Self::BoundaryWeb,
    ];

    pub fn attribute_value(self) -> &'static str {
        match self {
            Self::Plane => "plane",
            Self::FullVoid => "full_void",
            Self::EdgeVoid => "edge_void",
            Self::BoundaryWeb => "boundary_web",
        }
    }

    fn from_attribute_value(value: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.attribute_value() == value)
            .with_context(|| format!("unknown diode.copper_balance value '{value}'"))
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CopperBalanceMetadata {
    pub(super) void: Option<CopperBalanceVoidMetadata>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CopperBalanceVoidMetadata {
    pub(super) lattice_origin: Point,
    pub(super) lattice_pitch_mm: f64,
    pub(super) radius_mm: f64,
    pub(super) corner_radius_mm: f64,
}

pub(super) fn set_copper_balance_metadata(
    strings: &Interner,
    attributes: &[ipc2581::types::NonstandardAttribute],
) -> Result<Option<CopperBalanceMetadata>> {
    let kind = nonstandard_attribute(strings, attributes, COPPER_BALANCE_ATTRIBUTE_NAME, "STRING")?;
    let auxiliary_attributes = [
        (COPPER_BALANCE_LATTICE_ATTRIBUTE_NAME, "STRING"),
        (COPPER_BALANCE_LATTICE_ORIGIN_X_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_LATTICE_ORIGIN_Y_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_LATTICE_PITCH_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_VOID_RADIUS_ATTRIBUTE_NAME, "DOUBLE"),
        (COPPER_BALANCE_VOID_CORNER_RADIUS_ATTRIBUTE_NAME, "DOUBLE"),
    ];
    let Some(kind) = kind else {
        if auxiliary_attributes.iter().any(|(name, _)| {
            attributes
                .iter()
                .any(|attribute| strings.resolve(attribute.name) == *name)
        }) {
            bail!("copper-balance lattice metadata requires diode.copper_balance");
        }
        return Ok(None);
    };
    let kind = CopperBalanceKind::from_attribute_value(kind)?;
    let void = if kind == CopperBalanceKind::FullVoid {
        let lattice = required_nonstandard_attribute(
            strings,
            attributes,
            COPPER_BALANCE_LATTICE_ATTRIBUTE_NAME,
            "STRING",
        )?;
        if lattice != COPPER_BALANCE_LATTICE_VALUE {
            bail!("unsupported copper-balance lattice '{lattice}'");
        }
        let metadata = CopperBalanceVoidMetadata {
            lattice_origin: Point::new(
                required_double_attribute(
                    strings,
                    attributes,
                    COPPER_BALANCE_LATTICE_ORIGIN_X_ATTRIBUTE_NAME,
                )?,
                required_double_attribute(
                    strings,
                    attributes,
                    COPPER_BALANCE_LATTICE_ORIGIN_Y_ATTRIBUTE_NAME,
                )?,
            ),
            lattice_pitch_mm: required_double_attribute(
                strings,
                attributes,
                COPPER_BALANCE_LATTICE_PITCH_ATTRIBUTE_NAME,
            )?,
            radius_mm: required_double_attribute(
                strings,
                attributes,
                COPPER_BALANCE_VOID_RADIUS_ATTRIBUTE_NAME,
            )?,
            corner_radius_mm: required_double_attribute(
                strings,
                attributes,
                COPPER_BALANCE_VOID_CORNER_RADIUS_ATTRIBUTE_NAME,
            )?,
        };
        if !metadata.lattice_origin.x.is_finite()
            || !metadata.lattice_origin.y.is_finite()
            || !metadata.lattice_pitch_mm.is_finite()
            || metadata.lattice_pitch_mm <= 0.0
            || !metadata.radius_mm.is_finite()
            || metadata.radius_mm <= 0.0
            || !metadata.corner_radius_mm.is_finite()
            || metadata.corner_radius_mm <= 0.0
        {
            bail!("copper-balance lattice and rounded-hex dimensions must be finite and positive");
        }
        crate::geom::shapes::rounded_hexagon(metadata.radius_mm, metadata.corner_radius_mm, 0.0)
            .context("copper-balance rounded-hex dimensions are invalid")?;
        Some(metadata)
    } else {
        for (name, attribute_type) in auxiliary_attributes {
            if nonstandard_attribute(strings, attributes, name, attribute_type)?.is_some() {
                bail!("copper-balance {kind:?} set must not carry lattice metadata");
            }
        }
        None
    };
    Ok(Some(CopperBalanceMetadata { void }))
}

pub(super) fn required_double_attribute(
    strings: &Interner,
    attributes: &[ipc2581::types::NonstandardAttribute],
    name: &str,
) -> Result<f64> {
    let value = required_nonstandard_attribute(strings, attributes, name, "DOUBLE")?;
    value
        .parse::<f64>()
        .with_context(|| format!("{name} has invalid DOUBLE value '{value}'"))
}

pub(super) fn required_nonstandard_attribute<'a>(
    strings: &'a Interner,
    attributes: &'a [ipc2581::types::NonstandardAttribute],
    name: &str,
    expected_type: &str,
) -> Result<&'a str> {
    nonstandard_attribute(strings, attributes, name, expected_type)?
        .with_context(|| format!("copper-balance set is missing {name}"))
}

pub(super) fn nonstandard_attribute<'a>(
    strings: &'a Interner,
    attributes: &'a [ipc2581::types::NonstandardAttribute],
    name: &str,
    expected_type: &str,
) -> Result<Option<&'a str>> {
    let mut named = attributes
        .iter()
        .filter(|attribute| strings.resolve(attribute.name) == name);
    let Some(attribute) = named.next() else {
        return Ok(None);
    };
    if named.next().is_some() {
        bail!("{name} attribute occurs more than once");
    }
    let attr_type = attribute
        .attr_type
        .map(|attr_type| strings.resolve(attr_type))
        .with_context(|| format!("{name} attribute has no type"))?;
    if attr_type != expected_type {
        bail!("{name} attribute must have type {expected_type}, got '{attr_type}'");
    }
    attribute
        .value
        .map(|value| strings.resolve(value))
        .with_context(|| format!("{name} attribute has no value"))
        .map(Some)
}

/// Validate a full_void set against its declared lattice metadata: one
/// shared contour flashed by an identity-Xform placement group whose
/// locations are distinct on-lattice sites.
pub(super) fn validate_copper_balance_structure(
    metadata: Option<CopperBalanceMetadata>,
    set_feature: &SetFeature,
    features: &[GeometryFeature],
    doc: &GeometryDocument,
    resolution: Resolution,
) -> Result<()> {
    let Some(void) = metadata.and_then(|metadata| metadata.void) else {
        return Ok(());
    };
    let SetFeature::PlacementGroup(source_group) = set_feature else {
        bail!("copper-balance full_void set must contain one placement group");
    };
    if source_group.xform != Some(Xform::default()) {
        bail!("copper-balance lattice placement group must carry an identity Xform");
    }
    let [feature] = features else {
        bail!("copper-balance lattice placement group must contain one feature");
    };
    let group_id = feature
        .placement_group
        .context("copper-balance void feature has no placement group")?;
    let group = doc.feature_placement_groups[group_id as usize];
    if group.placements.len() != source_group.locations.len() {
        bail!("copper-balance lattice locations did not produce matching placements");
    }
    validate_copper_balance_void_shape(doc, feature, void, resolution)?;

    let lattice = crate::geom::copper_balance::DenseCopperLattice {
        origin: void.lattice_origin,
        pitch_mm: void.lattice_pitch_mm,
    };
    let mut sites = BTreeSet::new();
    for location in &source_group.locations {
        let point = Point::new(location.x, location.y);
        let (site, center) = lattice.nearest_site(point);
        if point.distance_to(center) > LATTICE_COORDINATE_TOLERANCE_MM {
            bail!(
                "copper-balance void location ({}, {}) is not on its declared lattice",
                point.x,
                point.y
            );
        }
        if !sites.insert((site.column, site.row)) {
            bail!("copper-balance lattice contains a duplicate site");
        }
    }
    Ok(())
}

pub(super) const LATTICE_COORDINATE_TOLERANCE_MM: f64 = 5e-5;

pub(super) fn validate_copper_balance_void_shape(
    doc: &GeometryDocument,
    feature: &GeometryFeature,
    metadata: CopperBalanceVoidMetadata,
    resolution: Resolution,
) -> Result<()> {
    let actual = crate::dialects::ipc::contour_flash_aperture(doc, feature)
        .context("copper-balance void is not one filled rigid contour")?;
    let crate::dialects::artwork::ApertureShape::Contour { outline, fill_rule } = actual else {
        bail!("copper-balance void did not lower to a contour aperture");
    };
    let expected =
        crate::geom::shapes::rounded_hexagon(metadata.radius_mm, metadata.corner_radius_mm, 0.0)
            .context("copper-balance rounded-hex dimensions are invalid")?;
    let resolution = resolution.with_tolerance(1e-5);
    let actual = ContourSet::from_contours(&[outline], fill_rule, resolution)?;
    let expected = ContourSet::from_contours(&[expected], FillRule::NonZero, resolution)?;
    let mismatch = actual.difference(&expected)?.area() + expected.difference(&actual)?.area();
    if mismatch > 1e-5 {
        bail!(
            "copper-balance void contour disagrees with its rounded-hex metadata by {mismatch} mm^2"
        );
    }
    Ok(())
}
