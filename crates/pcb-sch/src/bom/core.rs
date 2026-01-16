use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::natural_string::NaturalString;
use crate::{InstanceKind, PhysicalValue, Schematic};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bom {
    pub entries: HashMap<String, BomEntry>,   // path -> BomEntry
    pub designators: HashMap<String, String>, // path -> designator
    #[serde(skip)]
    pub availability: HashMap<String, AvailabilityData>, // path -> availability data
    #[serde(skip)]
    pub offers: HashMap<String, Vec<BomOffer>>, // path -> all offers from API
}

/// Offer data for JSON output - represents a single distributor offer
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BomOffer {
    pub region: String, // "us" or "global"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor_part_id: Option<String>,
    pub stock: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_price: Option<f64>, // unit price at 20x board qty
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
}

/// Per-region availability data for a single offer
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RegionAvailability {
    pub stock_total: i32,                      // Stock for selected (best) offer
    pub alt_stock_total: i32,                  // Combined stock from alternative offers
    pub price_breaks: Option<Vec<(i32, f64)>>, // Price breaks as (qty, unit_price)
    pub lcsc_part_ids: Vec<(String, String)>,  // Vec of (LCSC part number, product URL)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>, // Manufacturer part number from offer
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>, // Manufacturer from offer
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AvailabilityData {
    /// US region availability (best offer)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub us: Option<RegionAvailability>,
    /// Global region availability (best offer)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<RegionAvailability>,
}

/// Trim and truncate description to 100 chars max
fn trim_description(s: Option<String>) -> Option<String> {
    s.map(|s| {
        let trimmed = s.trim();
        if trimmed.len() > 100 {
            format!("{} ...", &trimmed[..96])
        } else {
            trimmed.to_string()
        }
    })
    .filter(|s| !s.is_empty())
}

/// Check if optional constraint A meets or exceeds B's requirement
/// Returns true if A is compatible with B (A can replace B)
fn meets_or_exceeds<T>(a: &Option<T>, b: &Option<T>, cmp: impl Fn(&T, &T) -> bool) -> bool {
    match (a, b) {
        (None, Some(_)) => false, // B requires, A doesn't have - incompatible
        (Some(_), None) => true,  // B doesn't require, A has it - OK
        (Some(va), Some(vb)) => cmp(va, vb), // Both have it - check if A meets B's requirement
        (None, None) => true,     // Neither constrained - compatible
    }
}

/// Format designators for logging
fn fmt_designators(s: &BTreeSet<NaturalString>) -> String {
    s.iter().map(|ns| ns.as_ref()).collect::<Vec<_>>().join(",")
}

/// Merge source designators into target and log the consolidation
fn merge_designators(target: &mut GroupedBomEntry, src: &GroupedBomEntry) {
    log::info!(
        "Consolidating BOM: Merging {} (loose spec: {}) into {} (strict spec: {})",
        fmt_designators(&src.designators),
        src.entry.description.as_deref().unwrap_or("?"),
        fmt_designators(&target.designators),
        target.entry.description.as_deref().unwrap_or("?")
    );
    target.designators.extend(src.designators.iter().cloned());
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupedBomEntry {
    pub designators: BTreeSet<NaturalString>,
    #[serde(flatten)]
    pub entry: BomEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Alternative {
    pub mpn: String,
    pub manufacturer: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BomEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mpn: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<Alternative>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generic_data: Option<GenericComponent>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dnp: bool,
    /// Whether this component should be excluded from BOM output (e.g., fiducials, test points)
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_bom: bool,
    /// BOM matcher function name (used for custom BOM matching logic)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// Additional properties from IPC-2581 textual characteristics
    #[serde(flatten)]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub properties: BTreeMap<String, String>,
}

impl BomEntry {
    pub fn matches_mpn(&self, mpn: &str) -> bool {
        // Check main MPN
        if let Some(entry_mpn) = &self.mpn {
            if entry_mpn == mpn {
                return true;
            }
        }

        // Check alternatives
        self.alternatives.iter().any(|alt| alt.mpn == mpn)
    }

    pub fn matches_generic(&self, key: &GenericMatchingKey) -> bool {
        // Check package compatibility
        if let Some(entry_package) = &self.package {
            if &key.package != entry_package {
                return false;
            }
        } else {
            // Entry has no package specified, cannot match a specific package requirement
            return false;
        }

        // Check component-specific matching
        if let Some(generic_data) = &self.generic_data {
            generic_data.matches(&key.component)
        } else {
            false
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UngroupedBomEntry {
    pub path: String,
    pub designator: String,
    #[serde(flatten)]
    pub entry: BomEntry,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub availability_tier: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub offers: Vec<BomOffer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "component_type")]
pub enum GenericComponent {
    Capacitor(Capacitor),
    Resistor(Resistor),
}

impl GenericComponent {
    pub fn matches(&self, key: &GenericComponent) -> bool {
        match (self, key) {
            (GenericComponent::Resistor(resistor), GenericComponent::Resistor(key_resistor)) => {
                resistor.matches(key_resistor)
            }
            (
                GenericComponent::Capacitor(capacitor),
                GenericComponent::Capacitor(key_capacitor),
            ) => capacitor.matches(key_capacitor),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Capacitor {
    pub capacitance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dielectric: Option<Dielectric>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub esr: Option<PhysicalValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage: Option<PhysicalValue>,
}

impl Capacitor {
    pub fn matches(&self, key: &Capacitor) -> bool {
        // Check capacitance range (key range must fit within component tolerance)
        if !key.capacitance.fits_within_default(&self.capacitance) {
            return false;
        }

        // Check voltage: key voltage must be > component voltage
        if let (Some(key_voltage), Some(component_voltage)) = (&key.voltage, &self.voltage) {
            if key_voltage.value > component_voltage.value {
                return false;
            }
        }

        // Check dielectric: key dielectric must match component dielectric
        if let (Some(key_dielec), Some(component_dielec)) = (&key.dielectric, &self.dielectric) {
            if key_dielec != component_dielec {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Resistor {
    pub resistance: PhysicalValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voltage: Option<PhysicalValue>,
}

impl Resistor {
    pub fn matches(&self, key: &Resistor) -> bool {
        // Check resistance range (key range must fit within component tolerance)
        if !key.resistance.fits_within_default(&self.resistance) {
            return false;
        }

        // Check voltage: key voltage must be > component voltage
        if let (Some(key_voltage), Some(component_voltage)) = (&key.voltage, &self.voltage) {
            if key_voltage.value > component_voltage.value {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Dielectric {
    C0G,
    NP0,
    X5R,
    X7R,
    X7S,
    X7T,
    Y5V,
    Z5U,
}

impl FromStr for Dielectric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "C0G" => Ok(Dielectric::C0G),
            "NP0" => Ok(Dielectric::NP0),
            "X5R" => Ok(Dielectric::X5R),
            "X7R" => Ok(Dielectric::X7R),
            "X7S" => Ok(Dielectric::X7S),
            "X7T" => Ok(Dielectric::X7T),
            "Y5V" => Ok(Dielectric::Y5V),
            "Z5U" => Ok(Dielectric::Z5U),
            _ => Err(format!("Unknown dielectric: {s}")),
        }
    }
}

// BOM Matching API
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BomMatchingKey {
    Mpn(String),
    Generic(GenericMatchingKey),
    Path(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenericMatchingKey {
    #[serde(flatten)]
    pub component: GenericComponent,
    pub package: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Offer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor_pn: Option<String>,
    pub manufacturer: Option<String>,
    pub manufacturer_pn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rank: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BomMatchingRule {
    pub key: BomMatchingKey,
    pub offers: Vec<Offer>,
}

impl Bom {
    /// Get the number of entries in the BOM
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the BOM is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Create a BOM from raw entries and designators
    pub fn new(entries: HashMap<String, BomEntry>, designators: HashMap<String, String>) -> Self {
        Bom {
            entries,
            designators,
            availability: HashMap::new(),
            offers: HashMap::new(),
        }
    }

    pub fn from_schematic(schematic: &Schematic) -> Self {
        let mut designators = HashMap::<String, String>::new();
        let mut entries = HashMap::<String, BomEntry>::new();

        schematic
            .instances
            .iter()
            .filter(|(_, instance)| instance.kind == InstanceKind::Component)
            .for_each(|(instance_ref, instance)| {
                let designator = instance.reference_designator.clone().unwrap();
                let path = instance_ref.instance_path.join(".");
                let bom_entry = BomEntry {
                    mpn: instance.mpn(),
                    manufacturer: instance.manufacturer(),
                    description: trim_description(instance.description()),
                    package: instance.package(),
                    value: instance.value(),
                    alternatives: instance.alternatives_attr(),
                    generic_data: detect_generic_component(instance),
                    dnp: instance.dnp(),
                    skip_bom: instance.skip_bom(),
                    matcher: instance.matcher(),
                    properties: BTreeMap::new(),
                };
                entries.insert(path.clone(), bom_entry);
                designators.insert(path, designator);
            });

        Bom {
            entries,
            designators,
            availability: HashMap::new(),
            offers: HashMap::new(),
        }
    }

    pub fn ungrouped_json(&self) -> String {
        use crate::bom::availability::{is_small_generic_passive, tier_for_stock, Tier};

        let mut entries = self
            .entries
            .iter()
            .map(|(path, entry)| {
                // Get offers for this path
                let offers = self.offers.get(path).cloned().unwrap_or_default();

                // Compute tier from availability data (same logic as table)
                let tier = self.availability.get(path).map(|avail| {
                    let is_small_passive = is_small_generic_passive(
                        entry.generic_data.as_ref(),
                        entry.package.as_deref(),
                    );

                    // Use best stock from either region
                    let us_stock = avail.us.as_ref().map(|r| r.stock_total).unwrap_or(0);
                    let global_stock = avail.global.as_ref().map(|r| r.stock_total).unwrap_or(0);
                    let best_stock = us_stock.max(global_stock);

                    let qty = 1; // For ungrouped entries, qty is always 1
                    let tier = tier_for_stock(best_stock, qty, is_small_passive);
                    match tier {
                        Tier::Plenty => "plenty",
                        Tier::Limited => "limited",
                        Tier::Insufficient => "insufficient",
                    }
                    .to_string()
                });

                UngroupedBomEntry {
                    path: path.clone(),
                    designator: self.designators[path].clone(),
                    entry: entry.clone(),
                    availability_tier: tier,
                    offers,
                }
            })
            .collect::<Vec<_>>();
        // Sort by DNP status first (non-DNP before DNP), then by designator naturally
        entries.sort_by(|a, b| match a.entry.dnp.cmp(&b.entry.dnp) {
            std::cmp::Ordering::Equal => natord::compare(&a.designator, &b.designator),
            other => other,
        });
        serde_json::to_string_pretty(&entries).unwrap()
    }

    pub fn grouped_json(&self) -> String {
        // Group entries by their BomEntry content
        let mut groups = HashMap::<BomEntry, BTreeSet<NaturalString>>::new();

        for (path, entry) in &self.entries {
            let group = groups.entry(entry.clone()).or_default();
            group.insert(self.designators[path].clone().into());
        }

        // Convert to vec
        let mut grouped_entries = groups
            .into_iter()
            .map(|(entry, designators)| GroupedBomEntry { entry, designators })
            .collect::<Vec<_>>();

        grouped_entries.sort_by(|a, b| {
            // Sort by DNP status first (non-DNP before DNP)
            match a.entry.dnp.cmp(&b.entry.dnp) {
                std::cmp::Ordering::Equal => {
                    // Within same DNP status, sort by first designator
                    // BTreeSet<NaturalString> maintains natural order, so first() is correct
                    a.designators
                        .iter()
                        .next()
                        .cmp(&b.designators.iter().next())
                }
                other => other,
            }
        });

        // Apply generic BOM consolidation pass
        grouped_entries = Self::consolidate_generic_entries(grouped_entries);

        serde_json::to_string_pretty(&grouped_entries).unwrap()
    }

    /// Filter out components that have skip_bom=true
    /// Returns a new Bom with excluded components removed
    pub fn filter_excluded(&self) -> Self {
        let entries: HashMap<_, _> = self
            .entries
            .iter()
            .filter(|(_, entry)| !entry.skip_bom)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let designators: HashMap<_, _> = entries
            .keys()
            .filter_map(|path| {
                self.designators
                    .get(path)
                    .map(|d| (path.clone(), d.clone()))
            })
            .collect();

        Bom {
            entries,
            designators,
            availability: HashMap::new(),
            offers: HashMap::new(),
        }
    }

    /// Generic BOM Consolidation Pass
    ///
    /// Merges less-constrained generic component entries into more-constrained ones
    /// to reduce unique BOM rows when safe to do so.
    ///
    /// # Strategy
    ///
    /// A part that meets stricter requirements automatically meets looser requirements.
    /// For example, a 1µF 10V capacitor can be used anywhere that needs "1µF" (no voltage spec).
    ///
    /// # Safety Rules
    ///
    /// Consolidation only happens when ALL of these conditions are met:
    ///
    /// 1. **Generic components only** - Both entries must have `generic_data` populated
    /// 2. **Identical component type** - Both resistors OR both capacitors
    /// 3. **Identical package** - Same footprint (e.g., both 0402)
    /// 4. **Identical DNP status** - Can't merge DNP with non-DNP
    /// 5. **Stricter entry has MPN** - The more-constrained entry must have a part assigned
    /// 6. **Compatible MPNs** - If both have MPNs, they must match (don't override user choice)
    ///
    /// # Strictness Comparison
    ///
    /// Entry A is "strictly more constrained" than Entry B if:
    ///
    /// **For Capacitors:**
    /// - Capacitance matches (within tolerance)
    /// - A has all constraints B has, plus at least one more:
    ///   - A has voltage ≥ B.voltage (or B has no voltage)
    ///   - A has same dielectric as B (or B has no dielectric)
    ///   - A has ESR ≤ B.esr (or B has no ESR)
    ///
    /// **For Resistors:**
    /// - Resistance matches (within tolerance)
    /// - A has all constraints B has, plus at least one more:
    ///   - A has voltage ≥ B.voltage (or B has no voltage)
    ///
    /// # Example
    ///
    /// Before:
    /// ```text
    /// 2 | C1,C2   | GRM155Z71A105KE01D | 1uF 10V
    /// 2 | C14,C15 | GRM155Z71A105KE01D | 1uF
    /// ```
    ///
    /// After:
    /// ```text
    /// 4 | C1,C2,C14,C15 | GRM155Z71A105KE01D | 1uF 10V
    /// ```
    ///
    pub fn consolidate_generic_entries(entries: Vec<GroupedBomEntry>) -> Vec<GroupedBomEntry> {
        use std::collections::HashMap;
        use std::mem::discriminant;

        let mut out = Vec::new();
        let mut groups: HashMap<
            (
                std::mem::Discriminant<GenericComponent>,
                Option<String>,
                bool,
            ),
            Vec<GroupedBomEntry>,
        > = HashMap::new();

        // Separate generic from non-generic entries
        for entry in entries {
            if let Some(g) = &entry.entry.generic_data {
                let key = (
                    discriminant(g),
                    entry.entry.package.clone(),
                    entry.entry.dnp,
                );
                groups.entry(key).or_default().push(entry);
            } else {
                out.push(entry);
            }
        }

        // Consolidate each group using absorb-into-representatives
        for mut group in groups.into_values() {
            let mut reps: Vec<GroupedBomEntry> = Vec::new();

            while let Some(mut cur) = group.pop() {
                let mut i = 0;
                let mut absorbed = false;

                while i < reps.len() {
                    match consolidate_order(&reps[i].entry, &cur.entry) {
                        Some(std::cmp::Ordering::Greater) => {
                            // reps[i] stricter - absorb cur into it
                            merge_designators(&mut reps[i], &cur);
                            absorbed = true;
                            break;
                        }
                        Some(std::cmp::Ordering::Less) => {
                            // cur stricter - absorb reps[i] into cur, remove rep
                            let victim = reps.remove(i);
                            merge_designators(&mut cur, &victim);
                            // Don't increment i - check next rep at same position
                        }
                        Some(std::cmp::Ordering::Equal) | None => {
                            i += 1;
                        }
                    }
                }

                if !absorbed {
                    reps.push(cur);
                }
            }

            out.extend(reps);
        }

        out
    }
}

/// Errors that can occur during KiCad BOM generation
#[derive(Debug, thiserror::Error)]
pub enum KiCadBomError {
    #[error("Failed to execute kicad-cli: {0}")]
    KiCadCliError(String),

    #[error("Failed to parse CSV: {0}")]
    CsvError(#[from] csv::Error),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}

/// Parse KiCad CSV BOM into our internal BOM structure
pub fn parse_kicad_csv_bom(csv_content: &str) -> Result<Bom, KiCadBomError> {
    let mut reader = csv::Reader::from_reader(csv_content.as_bytes());
    let mut entries = HashMap::new();
    let mut designators = HashMap::new();

    for result in reader.records() {
        let record = result?;

        if record.is_empty() {
            continue;
        }

        // Get fields by position (matching our kicad-cli labels order)
        let reference = record.get(0).unwrap_or("").trim();
        let value = record.get(1).unwrap_or("").trim();
        let footprint = record.get(2).unwrap_or("").trim();
        let manufacturer = record.get(3).unwrap_or("").trim();
        let mpn = record.get(4).unwrap_or("").trim();
        let description = record.get(5).unwrap_or("").trim();
        let dnp = record.get(6).unwrap_or("").trim();

        // Skip power symbols and net labels
        if reference.is_empty() || reference.starts_with('#') {
            continue;
        }

        let path = format!("kicad::{}", reference);

        // Helper to convert empty string to None
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_string());

        let entry = BomEntry {
            mpn: non_empty(mpn).or_else(|| {
                // Use Value as MPN if it looks like a part number (no spaces)
                non_empty(value).filter(|v| !v.contains(' '))
            }),
            alternatives: Vec::new(),
            manufacturer: non_empty(manufacturer),
            package: non_empty(footprint).map(|fp| {
                // Remove library prefix (e.g., "Lib:Package" -> "Package")
                fp.split(':').next_back().unwrap_or(&fp).to_string()
            }),
            value: non_empty(value),
            description: non_empty(description),
            generic_data: None,
            dnp: dnp == "DNP" || dnp.to_lowercase() == "yes" || dnp == "1",
            skip_bom: false, // KiCad CSV exports don't include this field
            matcher: None,
            properties: BTreeMap::new(),
        };

        entries.insert(path.clone(), entry);
        designators.insert(path, reference.to_string());
    }

    Ok(Bom {
        entries,
        designators,
        availability: HashMap::new(),
        offers: HashMap::new(),
    })
}

/// Detect generic components based on Type attribute
/// Compare two BOM entries for consolidation ordering
///
/// Returns Some(Ordering) indicating which entry is stricter, or None if not safe to consolidate
fn consolidate_order(a: &BomEntry, b: &BomEntry) -> Option<std::cmp::Ordering> {
    let ga = a.generic_data.as_ref()?;
    let gb = b.generic_data.as_ref()?;

    // Safety checks
    if std::mem::discriminant(ga) != std::mem::discriminant(gb) {
        return None;
    }
    if a.package != b.package {
        return None;
    }
    if a.dnp != b.dnp {
        return None;
    }

    // Check if components can replace each other
    let a_meets_b = component_meets_or_exceeds(ga, gb);
    let b_meets_a = component_meets_or_exceeds(gb, ga);

    let ord = match (a_meets_b, b_meets_a) {
        (true, false) => std::cmp::Ordering::Greater, // A meets B but not vice versa - A is stricter
        (false, true) => std::cmp::Ordering::Less, // B meets A but not vice versa - B is stricter
        (true, true) => std::cmp::Ordering::Equal, // Equivalent specs - either can replace the other
        (false, false) => return None,             // Incompatible - can't consolidate
    };

    // MPN rules
    if let (Some(ma), Some(mb)) = (&a.mpn, &b.mpn) {
        if ma != mb {
            return None;
        }
    }

    // Consolidation rules based on ordering
    match ord {
        std::cmp::Ordering::Greater if a.mpn.is_some() => Some(std::cmp::Ordering::Greater),
        std::cmp::Ordering::Less if b.mpn.is_some() => Some(std::cmp::Ordering::Less),
        std::cmp::Ordering::Equal if a.mpn.is_some() || b.mpn.is_some() => {
            // Equal specs - consolidate into whichever has MPN
            if a.mpn.is_some() {
                Some(std::cmp::Ordering::Greater)
            } else {
                Some(std::cmp::Ordering::Less)
            }
        }
        _ => None,
    }
}

/// Check if generic component A meets or exceeds B's requirements
///
/// Returns true if A can replace B (A's specs meet or exceed all of B's requirements)
fn component_meets_or_exceeds(a: &GenericComponent, b: &GenericComponent) -> bool {
    match (a, b) {
        (GenericComponent::Resistor(res_a), GenericComponent::Resistor(res_b)) => {
            resistor_meets_or_exceeds(res_a, res_b)
        }
        (GenericComponent::Capacitor(cap_a), GenericComponent::Capacitor(cap_b)) => {
            capacitor_meets_or_exceeds(cap_a, cap_b)
        }
        _ => false, // Different types can't be compared
    }
}

/// Check if resistor A meets or exceeds resistor B's requirements
fn resistor_meets_or_exceeds(a: &Resistor, b: &Resistor) -> bool {
    // A's resistance range must fit within B's (A's tolerance is same or tighter)
    if !a.resistance.fits_within_default(&b.resistance) {
        return false;
    }

    // A's voltage rating must meet or exceed B's (A has same or higher voltage rating)
    meets_or_exceeds(&a.voltage, &b.voltage, |va, vb| va.value >= vb.value)
}

/// Check if capacitor A meets or exceeds capacitor B's requirements
fn capacitor_meets_or_exceeds(a: &Capacitor, b: &Capacitor) -> bool {
    // A's capacitance range must fit within B's (A's tolerance is same or tighter)
    if !a.capacitance.fits_within_default(&b.capacitance) {
        return false;
    }

    // Check all optional constraints - A must meet or exceed each of B's requirements
    meets_or_exceeds(&a.voltage, &b.voltage, |va, vb| va.value >= vb.value)
        && meets_or_exceeds(&a.dielectric, &b.dielectric, |da, db| da == db)
        && meets_or_exceeds(&a.esr, &b.esr, |ea, eb| ea.value <= eb.value)
}

fn detect_generic_component(instance: &crate::Instance) -> Option<GenericComponent> {
    match instance.component_type()?.as_str() {
        "resistor" => {
            if let Some(resistance) = instance.physical_attr(&["Resistance", "resistance"]) {
                let voltage = instance.physical_attr(&["Voltage", "voltage"]);
                return Some(GenericComponent::Resistor(Resistor {
                    resistance,
                    voltage,
                }));
            }
        }
        "capacitor" => {
            if let Some(capacitance) = instance.physical_attr(&["Capacitance", "capacitance"]) {
                let dielectric = instance
                    .string_attr(&["Dielectric", "dielectric"])
                    .and_then(|d| d.parse().ok());

                let esr = instance.physical_attr(&["ESR", "esr", "Esr"]);
                let voltage = instance.physical_attr(&["Voltage", "voltage"]);

                return Some(GenericComponent::Capacitor(Capacitor {
                    capacitance,
                    dielectric,
                    esr,
                    voltage,
                }));
            }
        }
        _ => {}
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AttributeValue, Instance, ModuleRef, PhysicalUnit};
    use rust_decimal::prelude::FromPrimitive;
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn test_instance(attributes: HashMap<String, AttributeValue>) -> Instance {
        Instance {
            type_ref: ModuleRef {
                source_path: PathBuf::new(),
                module_name: String::default(),
            },
            kind: InstanceKind::Component,
            attributes,
            children: Default::default(),
            reference_designator: Some("U1".to_string()),
            symbol_positions: HashMap::new(),
        }
    }

    #[test]
    fn test_detect_generic_component() {
        // Create a mock resistor with Type attribute
        let mut attributes = HashMap::new();
        attributes.insert(
            "Type".to_string(),
            AttributeValue::String("resistor".to_string()),
        );
        attributes.insert(
            "resistance".to_string(),
            AttributeValue::String("10k 1%".to_string()),
        );

        let instance = test_instance(attributes);
        let result = detect_generic_component(&instance);

        match result {
            Some(GenericComponent::Resistor(resistor)) => {
                assert_eq!(
                    resistor.resistance.value,
                    Decimal::from_f64(10000.0).unwrap()
                );
                assert_eq!(
                    resistor.resistance.tolerance,
                    Decimal::from_f64(0.01).unwrap()
                );
            }
            _ => panic!("Expected resistor module"),
        }

        // Test capacitor detection
        let mut capacitor_attributes = HashMap::new();
        capacitor_attributes.insert(
            "Type".to_string(),
            AttributeValue::String("capacitor".to_string()),
        );
        capacitor_attributes.insert(
            "capacitance".to_string(),
            AttributeValue::String("100nF 20%".to_string()),
        );
        capacitor_attributes.insert(
            "Dielectric".to_string(),
            AttributeValue::String("X7R".to_string()),
        );

        let instance = test_instance(capacitor_attributes);
        let result = detect_generic_component(&instance);

        match result {
            Some(GenericComponent::Capacitor(capacitor)) => {
                let expected_value = Decimal::from_f64(100e-9).unwrap();
                assert!(
                    (capacitor.capacitance.value - expected_value).abs()
                        < Decimal::from_f64(1e-15).unwrap()
                );
                assert_eq!(
                    capacitor.capacitance.tolerance,
                    Decimal::from_f64(0.2).unwrap()
                );
                assert_eq!(capacitor.dielectric, Some(Dielectric::X7R));
            }
            _ => panic!("Expected capacitor module"),
        }
    }

    #[test]
    fn test_tagged_serde() {
        // Test that serde can distinguish between modules using component_type tag

        // Resistor should deserialize with component_type tag
        let resistor_json = r#"{
            "component_type": "Resistor",
            "resistance": {"value": "10000.0", "tolerance": "0.01", "unit": "Ohms"}
        }"#;

        let resistor: GenericComponent = serde_json::from_str(resistor_json).unwrap();
        match resistor {
            GenericComponent::Resistor(r) => {
                assert_eq!(r.resistance.value, Decimal::from_f64(10000.0).unwrap());
                assert_eq!(r.resistance.tolerance, Decimal::from_f64(0.01).unwrap());
            }
            _ => panic!("Expected Resistor variant"),
        }

        // Capacitor should deserialize with component_type tag
        let capacitor_json = r#"{
            "component_type": "Capacitor",
            "capacitance": {"value": "100e-9", "tolerance": "0.2", "unit": "Farads"},
            "dielectric": "X7R"
        }"#;

        let capacitor: GenericComponent = serde_json::from_str(capacitor_json).unwrap();
        match capacitor {
            GenericComponent::Capacitor(c) => {
                let expected_value = Decimal::from_f64(100e-9).unwrap();
                assert!(
                    (c.capacitance.value - expected_value).abs()
                        < Decimal::from_f64(1e-15).unwrap()
                );
                assert_eq!(c.capacitance.tolerance, Decimal::from_f64(0.2).unwrap());
                assert_eq!(c.dielectric, Some(Dielectric::X7R));
            }
            _ => panic!("Expected Capacitor variant"),
        }

        // Test round-trip serialization
        let original_resistor = GenericComponent::Resistor(Resistor {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
            voltage: None,
        });

        let json = serde_json::to_string_pretty(&original_resistor).unwrap();
        let deserialized: GenericComponent = serde_json::from_str(&json).unwrap();
        assert_eq!(original_resistor, deserialized);
    }

    #[test]
    fn test_resistor_matching() {
        // Component: 1kΩ ±0% (defaults to ±1%)
        let component_resistor = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Ohms),
            voltage: None,
        };

        // Key: 1kΩ ±1% - should match (exact fit)
        let matching_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(component_resistor.matches(&matching_key));

        // Key: 1kΩ ±0.5% - should match (tighter tolerance fits)
        let tighter_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.005, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(component_resistor.matches(&tighter_key));

        // Key: 1kΩ ±5% - should NOT match (looser tolerance doesn't fit)
        let looser_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.05, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(!component_resistor.matches(&looser_key));

        // Key: 2kΩ ±1% - should NOT match (different value)
        let different_value_key = Resistor {
            resistance: PhysicalValue::new(2000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        assert!(!component_resistor.matches(&different_value_key));
    }

    #[test]
    fn test_resistor_voltage_matching() {
        let component_resistor = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(50.0, 0.0, PhysicalUnit::Volts)),
        };

        // Key voltage (25V) <= component voltage (50V) - should match
        let lower_voltage_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(25.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(component_resistor.matches(&lower_voltage_key));

        // Key voltage (100V) > component voltage (50V) - should NOT match
        let higher_voltage_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(100.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(!component_resistor.matches(&higher_voltage_key));

        // No component voltage specified - should match any key voltage
        let no_voltage_component = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: None,
        };
        let any_voltage_key = Resistor {
            resistance: PhysicalValue::new(1000.0, 0.01, PhysicalUnit::Ohms),
            voltage: Some(PhysicalValue::new(1000.0, 0.0, PhysicalUnit::Volts)),
        };
        assert!(no_voltage_component.matches(&any_voltage_key));
    }

    #[test]
    fn test_capacitor_matching() {
        // Component: 100nF ±10% X7R
        let component_capacitor = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            dielectric: Some(Dielectric::X7R),
            esr: None,
            voltage: None,
        };

        // Key: 100nF ±10% X7R - should match (exact)
        let matching_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(component_capacitor.matches(&matching_key));

        // Key: 100nF ±5% X7R - should match (tighter tolerance)
        let tighter_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.05, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(component_capacitor.matches(&tighter_key));

        // Key: 100nF ±20% X7R - should NOT match (looser tolerance)
        let looser_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.2, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(!component_capacitor.matches(&looser_key));

        // Key: 100nF ±10% C0G - should NOT match (different dielectric)
        let different_dielectric_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::C0G),
            esr: None,
        };
        assert!(!component_capacitor.matches(&different_dielectric_key));

        // Key: No dielectric specified - should match (no requirement)
        let no_dielectric_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: None,
            esr: None,
        };
        assert!(component_capacitor.matches(&no_dielectric_key));
    }

    #[test]
    fn test_capacitor_no_dielectric_component() {
        // Component: 100nF ±10% (no dielectric specified)
        let component_capacitor = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            dielectric: None,
            esr: None,
            voltage: None,
        };

        // Key: Any dielectric specified - should match (no component requirement)
        let x7r_key = Capacitor {
            capacitance: PhysicalValue::new(100e-9, 0.1, PhysicalUnit::Farads),
            voltage: None,
            dielectric: Some(Dielectric::X7R),
            esr: None,
        };
        assert!(component_capacitor.matches(&x7r_key));
    }

    // ============================================================================
    // Generic BOM Consolidation Tests
    // ============================================================================

    #[test]
    fn test_consolidate_capacitor_voltage_stricter() {
        // 1µF 10V should consolidate with 1µF (no voltage)
        let cap_10v = BomEntry {
            mpn: Some("PART-10V".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF 10V".to_string()),
            description: Some("1uF 10V".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: Some(PhysicalValue::new(10.0, 0.0, PhysicalUnit::Volts)),
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let cap_no_voltage = BomEntry {
            mpn: Some("PART-10V".to_string()), // Same MPN
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF".to_string()),
            description: Some("1uF".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: cap_10v,
                designators: BTreeSet::from(["C1".into(), "C2".into()]),
            },
            GroupedBomEntry {
                entry: cap_no_voltage,
                designators: BTreeSet::from(["C14".into(), "C15".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should consolidate into one entry
        assert_eq!(consolidated.len(), 1);
        assert_eq!(consolidated[0].designators.len(), 4);
        assert!(consolidated[0]
            .designators
            .contains(&NaturalString::from("C1")));
        assert!(consolidated[0]
            .designators
            .contains(&NaturalString::from("C2")));
        assert!(consolidated[0]
            .designators
            .contains(&NaturalString::from("C14")));
        assert!(consolidated[0]
            .designators
            .contains(&NaturalString::from("C15")));
        assert_eq!(
            consolidated[0].entry.description,
            Some("1uF 10V".to_string())
        );
    }

    #[test]
    fn test_consolidate_resistor_voltage_stricter() {
        // 10kΩ 100V should consolidate with 10kΩ (no voltage)
        let res_100v = BomEntry {
            mpn: Some("RES-100V".to_string()),
            manufacturer: Some("Yageo".to_string()),
            package: Some("0603".to_string()),
            value: Some("10k 100V".to_string()),
            description: Some("10k 100V".to_string()),
            generic_data: Some(GenericComponent::Resistor(Resistor {
                resistance: PhysicalValue::new(10000.0, 0.0, PhysicalUnit::Ohms),
                voltage: Some(PhysicalValue::new(100.0, 0.0, PhysicalUnit::Volts)),
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let res_no_voltage = BomEntry {
            mpn: Some("RES-100V".to_string()),
            manufacturer: Some("Yageo".to_string()),
            package: Some("0603".to_string()),
            value: Some("10k".to_string()),
            description: Some("10k".to_string()),
            generic_data: Some(GenericComponent::Resistor(Resistor {
                resistance: PhysicalValue::new(10000.0, 0.0, PhysicalUnit::Ohms),
                voltage: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: res_100v,
                designators: BTreeSet::from(["R1".into()]),
            },
            GroupedBomEntry {
                entry: res_no_voltage,
                designators: BTreeSet::from(["R2".into(), "R3".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        assert_eq!(consolidated.len(), 1);
        assert_eq!(consolidated[0].designators.len(), 3);
        assert_eq!(
            consolidated[0].entry.description,
            Some("10k 100V".to_string())
        );
    }

    #[test]
    fn test_no_consolidate_different_packages() {
        // 1µF 0402 and 1µF 0603 should NOT consolidate
        let cap_0402 = BomEntry {
            mpn: Some("PART-0402".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF".to_string()),
            description: Some("1uF".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let cap_0603 = BomEntry {
            mpn: Some("PART-0603".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0603".to_string()),
            value: Some("1uF".to_string()),
            description: Some("1uF".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: cap_0402,
                designators: BTreeSet::from(["C1".into()]),
            },
            GroupedBomEntry {
                entry: cap_0603,
                designators: BTreeSet::from(["C2".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should NOT consolidate - different packages
        assert_eq!(consolidated.len(), 2);
    }

    #[test]
    fn test_no_consolidate_different_dnp_status() {
        // DNP and non-DNP should NOT consolidate
        let cap_normal = BomEntry {
            mpn: Some("PART-A".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF".to_string()),
            description: Some("1uF".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let cap_dnp = BomEntry {
            dnp: true, // Different DNP status
            ..cap_normal.clone()
        };

        let entries = vec![
            GroupedBomEntry {
                entry: cap_normal,
                designators: BTreeSet::from(["C1".into()]),
            },
            GroupedBomEntry {
                entry: cap_dnp,
                designators: BTreeSet::from(["C2".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should NOT consolidate - different DNP status
        assert_eq!(consolidated.len(), 2);
    }

    #[test]
    fn test_no_consolidate_different_mpns() {
        // Different MPNs should NOT consolidate (user chose different parts)
        let cap_a = BomEntry {
            mpn: Some("PART-A".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF 10V".to_string()),
            description: Some("1uF 10V".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: Some(PhysicalValue::new(10.0, 0.0, PhysicalUnit::Volts)),
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let cap_b = BomEntry {
            mpn: Some("PART-B".to_string()), // Different MPN
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF".to_string()),
            description: Some("1uF".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: cap_a,
                designators: BTreeSet::from(["C1".into()]),
            },
            GroupedBomEntry {
                entry: cap_b,
                designators: BTreeSet::from(["C2".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should NOT consolidate - user chose different parts
        assert_eq!(consolidated.len(), 2);
    }

    #[test]
    fn test_no_consolidate_without_mpn() {
        // Stricter entry without MPN should NOT consolidate
        let cap_10v_no_mpn = BomEntry {
            mpn: None, // No MPN
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF 10V".to_string()),
            description: Some("1uF 10V".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: Some(PhysicalValue::new(10.0, 0.0, PhysicalUnit::Volts)),
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let cap_no_voltage = BomEntry {
            mpn: Some("PART-A".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF".to_string()),
            description: Some("1uF".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: cap_10v_no_mpn,
                designators: BTreeSet::from(["C1".into()]),
            },
            GroupedBomEntry {
                entry: cap_no_voltage,
                designators: BTreeSet::from(["C2".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should NOT consolidate - stricter entry has no MPN
        assert_eq!(consolidated.len(), 2);
    }

    #[test]
    fn test_consolidate_capacitor_dielectric_stricter() {
        // 1µF X7R should consolidate with 1µF (no dielectric)
        let cap_x7r = BomEntry {
            mpn: Some("CAP-X7R".to_string()),
            manufacturer: Some("Samsung".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF X7R".to_string()),
            description: Some("1uF X7R".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: Some(Dielectric::X7R),
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let cap_no_dielec = BomEntry {
            mpn: Some("CAP-X7R".to_string()),
            manufacturer: Some("Samsung".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF".to_string()),
            description: Some("1uF".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.0, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: cap_x7r,
                designators: BTreeSet::from(["C1".into()]),
            },
            GroupedBomEntry {
                entry: cap_no_dielec,
                designators: BTreeSet::from(["C2".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        assert_eq!(consolidated.len(), 1);
        assert_eq!(
            consolidated[0].entry.description,
            Some("1uF X7R".to_string())
        );
    }

    #[test]
    fn test_no_consolidate_non_generic() {
        // Non-generic components should NOT consolidate
        let entry_a = BomEntry {
            mpn: Some("IC-A".to_string()),
            manufacturer: Some("TI".to_string()),
            package: Some("SOIC-8".to_string()),
            value: Some("TPS82140".to_string()),
            description: Some("3A Buck Converter".to_string()),
            generic_data: None, // No generic data
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: entry_a.clone(),
                designators: BTreeSet::from(["U1".into()]),
            },
            GroupedBomEntry {
                entry: entry_a.clone(),
                designators: BTreeSet::from(["U2".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should NOT try to consolidate (though they'd be grouped by hash already)
        assert_eq!(consolidated.len(), 2);
    }

    #[test]
    fn test_consolidate_resistor_tighter_tolerance_different_nominal() {
        // 50Ω ±1% should consolidate with 52Ω ±10%
        // 50Ω ±1% = 49.5-50.5Ω, fits within 52Ω ±10% = 46.8-57.2Ω
        let res_tight = BomEntry {
            mpn: Some("RES-TIGHT".to_string()),
            manufacturer: Some("Yageo".to_string()),
            package: Some("0603".to_string()),
            value: Some("50 1%".to_string()),
            description: Some("50 1%".to_string()),
            generic_data: Some(GenericComponent::Resistor(Resistor {
                resistance: PhysicalValue::new(50.0, 0.01, PhysicalUnit::Ohms),
                voltage: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let res_loose = BomEntry {
            mpn: Some("RES-TIGHT".to_string()), // Same MPN - we're using the tighter spec part
            manufacturer: Some("Yageo".to_string()),
            package: Some("0603".to_string()),
            value: Some("52 10%".to_string()),
            description: Some("52 10%".to_string()),
            generic_data: Some(GenericComponent::Resistor(Resistor {
                resistance: PhysicalValue::new(52.0, 0.1, PhysicalUnit::Ohms),
                voltage: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: res_tight,
                designators: BTreeSet::from(["R1".into()]),
            },
            GroupedBomEntry {
                entry: res_loose,
                designators: BTreeSet::from(["R2".into(), "R3".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should consolidate to the tighter spec (50Ω ±1%)
        assert_eq!(consolidated.len(), 1);
        assert_eq!(consolidated[0].designators.len(), 3);
        assert_eq!(consolidated[0].entry.description, Some("50 1%".to_string()));
    }

    #[test]
    fn test_consolidate_capacitor_tighter_tolerance() {
        // 1µF ±5% should consolidate with 1µF ±20%
        let cap_tight = BomEntry {
            mpn: Some("CAP-TIGHT".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF 5%".to_string()),
            description: Some("1uF 5%".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.05, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let cap_loose = BomEntry {
            mpn: Some("CAP-TIGHT".to_string()),
            manufacturer: Some("Murata".to_string()),
            package: Some("0402".to_string()),
            value: Some("1uF 20%".to_string()),
            description: Some("1uF 20%".to_string()),
            generic_data: Some(GenericComponent::Capacitor(Capacitor {
                capacitance: PhysicalValue::new(1e-6, 0.2, PhysicalUnit::Farads),
                voltage: None,
                dielectric: None,
                esr: None,
            })),
            dnp: false,
            alternatives: vec![],
            skip_bom: false,
            matcher: None,
            properties: BTreeMap::new(),
        };

        let entries = vec![
            GroupedBomEntry {
                entry: cap_tight,
                designators: BTreeSet::from(["C1".into()]),
            },
            GroupedBomEntry {
                entry: cap_loose,
                designators: BTreeSet::from(["C2".into()]),
            },
        ];

        let consolidated = Bom::consolidate_generic_entries(entries);

        // Should consolidate to the tighter tolerance
        assert_eq!(consolidated.len(), 1);
        assert_eq!(
            consolidated[0].entry.description,
            Some("1uF 5%".to_string())
        );
    }
}
