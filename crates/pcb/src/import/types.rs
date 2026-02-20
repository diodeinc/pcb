use anyhow::Result;
use clap::Args;
use pcb_zen_core::Diagnostics;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct KiCadRefDes(String);

impl KiCadRefDes {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadRefDes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadRefDes {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct KiCadNetName(String);

impl KiCadNetName {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadNetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadNetName {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct KiCadPinNumber(String);

impl KiCadPinNumber {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadPinNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadPinNumber {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct KiCadLibId(String);

impl KiCadLibId {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for KiCadLibId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for KiCadLibId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[derive(Args, Debug, Clone)]
#[command(about = "Import KiCad projects into a Zener workspace")]
pub struct ImportArgs {
    /// Path to a KiCad project file (.kicad_pro)
    #[arg(value_name = "KICAD_PRO", value_hint = clap::ValueHint::AnyPath)]
    pub kicad_pro: PathBuf,

    /// Output directory (a workspace will be created if needed)
    #[arg(value_name = "OUTPUT_DIR", value_hint = clap::ValueHint::AnyPath)]
    pub output_dir: PathBuf,

    /// Overwrite existing board directory if it already exists
    #[arg(long = "force")]
    pub force: bool,
}

pub(super) struct ImportPaths {
    pub(super) workspace_root: PathBuf,
    pub(super) kicad_project_root: PathBuf,
    pub(super) kicad_pro_abs: PathBuf,
}

#[derive(Debug, Clone)]
pub(super) struct PortableKicadProject {
    pub(super) project_dir: PathBuf, // absolute path
    pub(super) project_name: String,
    pub(super) kicad_pro_rel: PathBuf, // relative to project_dir
    pub(super) root_schematic_rel: PathBuf, // relative to project_dir
    pub(super) primary_kicad_pcb_rel: PathBuf, // relative to project_dir
    pub(super) schematic_files_rel: Vec<PathBuf>,
    pub(super) files_to_bundle_rel: Vec<PathBuf>,
    pub(super) extra_files_to_bundle: Vec<PortableExtraFile>,
    pub(super) manifest_json: String,
}

#[derive(Debug, Clone)]
pub(super) struct PortableExtraFile {
    pub(super) source_path: PathBuf,          // absolute path
    pub(super) archive_relative_path: String, // archive-root relative path
}

pub(super) struct ImportSelection {
    pub(super) board_name: String,
    pub(super) board_name_source: BoardNameSource,
    pub(super) files: KicadDiscoveredFiles,
    pub(super) selected: SelectedKicadFiles,
    pub(super) portable: PortableKicadProject,
}

pub(super) struct ImportIr {
    pub(super) components: BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    pub(super) nets: BTreeMap<KiCadNetName, ImportNetData>,
    pub(super) schematic_lib_symbols: BTreeMap<KiCadLibId, String>,
    pub(super) schematic_power_symbol_decls: Vec<ImportSchematicPowerSymbolDecl>,
    pub(super) schematic_sheet_tree: ImportSheetTree,
    pub(super) hierarchy_plan: ImportHierarchyPlan,
    pub(super) semantic: ImportSemanticAnalysis,
}

pub(super) struct MaterializedBoard {
    pub(super) board_dir: PathBuf,
    pub(super) board_zen: PathBuf,
    pub(super) layout_dir: PathBuf,
    pub(super) layout_kicad_pro: PathBuf,
    pub(super) layout_kicad_pcb: PathBuf,
    pub(super) portable_kicad_project_zip: PathBuf,
    pub(super) validation_diagnostics_json: PathBuf,
    pub(super) import_extraction_json: PathBuf,
}

#[derive(Debug, Serialize)]
pub(super) struct ImportReport {
    pub(super) workspace_root: PathBuf,
    pub(super) kicad_project_root: PathBuf,
    pub(super) board_name: Option<String>,
    pub(super) board_name_source: Option<BoardNameSource>,
    pub(super) files: KicadDiscoveredFiles,
    pub(super) extraction: Option<ImportExtractionReport>,
    pub(super) validation: Option<ImportValidation>,
    pub(super) generated: Option<GeneratedArtifacts>,
}

#[derive(Debug, Serialize)]
pub(super) struct ImportExtractionReport {
    /// Netlist is the primary source-of-truth for component identities during import.
    ///
    /// Keys serialize to the derived KiCad PCB footprint `(path "...")` strings.
    pub(super) netlist_components: BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    /// Netlist-derived connectivity for each KiCad net.
    ///
    /// Keys are KiCad net names.
    pub(super) netlist_nets: BTreeMap<KiCadNetName, ImportNetData>,

    /// Embedded library symbols found in `.kicad_sch` files.
    ///
    /// We intentionally do not serialize the full symbol S-expressions in this report.
    pub(super) schematic_lib_symbol_ids: BTreeSet<KiCadLibId>,

    /// Schematic `(power)` symbol instances discovered in `.kicad_sch` files.
    ///
    /// These symbols typically do not appear as nodes in the KiCad netlist export, but their
    /// `Value` property declares a global net name which we can use as explicit intent for
    /// classifying `Power`/`Ground` nets.
    pub(super) schematic_power_symbol_decls: Vec<ImportSchematicPowerSymbolDecl>,

    /// Schematic sheet-instance tree keyed by KiCad sheetpath UUID chains.
    pub(super) schematic_sheet_tree: ImportSheetTree,

    /// Derived hierarchical net ownership and module boundary nets.
    pub(super) hierarchy_plan: ImportHierarchyPlan,

    /// Semantic analysis results derived from extracted KiCad artifacts.
    pub(super) semantic: ImportSemanticAnalysis,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct ImportSemanticAnalysis {
    pub(super) passives: ImportPassiveAnalysis,
    pub(super) net_kinds: ImportNetKindAnalysis,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct ImportNetKindAnalysis {
    /// Per-net kind classification keyed by KiCad net name.
    pub(super) by_net: BTreeMap<KiCadNetName, ImportNetKindClassification>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportNetKindClassification {
    pub(super) kind: ImportNetKind,
    pub(super) reasons: BTreeSet<String>,
}

#[derive(Debug, Default, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(super) enum ImportNetKind {
    #[default]
    Net,
    Power,
    Ground,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportSchematicPowerSymbolDecl {
    pub(super) schematic_file: PathBuf,
    pub(super) sheet_path: KiCadSheetPath,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) symbol_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) at: Option<ImportSchematicAt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) mirror: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) reference: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) lib_id: Option<KiCadLibId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) value: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct ImportPassiveAnalysis {
    /// Per-instance passive classification keyed by KiCad UUID path key.
    pub(super) by_component: BTreeMap<KiCadUuidPathKey, ImportPassiveClassification>,
    pub(super) summary: ImportPassiveSummary,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct ImportPassiveSummary {
    pub(super) resistor_high: usize,
    pub(super) resistor_medium: usize,
    pub(super) resistor_low: usize,
    pub(super) capacitor_high: usize,
    pub(super) capacitor_medium: usize,
    pub(super) capacitor_low: usize,
    pub(super) unknown: usize,
    pub(super) non_two_pad: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportPassiveClassification {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) kind: Option<ImportPassiveKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) confidence: Option<ImportPassiveConfidence>,
    pub(super) pad_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) package: Option<ImportPassivePackage>,
    /// Parsed resistance/capacitance value (suitable for `Resistance("...")` / `Capacitance("...")`)
    /// when we can infer it confidently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parsed_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) mpn: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) tolerance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) voltage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) dielectric: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) power: Option<String>,
    pub(super) signals: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ImportPassiveKind {
    Resistor,
    Capacitor,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub(super) enum ImportPassiveConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ImportPassivePackage {
    #[serde(rename = "01005")]
    P01005,
    #[serde(rename = "0201")]
    P0201,
    #[serde(rename = "0402")]
    P0402,
    #[serde(rename = "0603")]
    P0603,
    #[serde(rename = "0805")]
    P0805,
    #[serde(rename = "1206")]
    P1206,
    #[serde(rename = "1210")]
    P1210,
}

impl ImportPassivePackage {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            ImportPassivePackage::P01005 => "01005",
            ImportPassivePackage::P0201 => "0201",
            ImportPassivePackage::P0402 => "0402",
            ImportPassivePackage::P0603 => "0603",
            ImportPassivePackage::P0805 => "0805",
            ImportPassivePackage::P1206 => "1206",
            ImportPassivePackage::P1210 => "1210",
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct ImportHierarchyPlan {
    /// For each net, the sheet instance that "owns" it (the LCA of connected ports' sheet paths).
    pub(super) net_owner: BTreeMap<KiCadNetName, KiCadSheetPath>,
    /// For each sheet path, the derived net sets needed to generate a sheet module.
    pub(super) modules: BTreeMap<KiCadSheetPath, ImportModuleBoundaryNets>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(super) struct ImportModuleBoundaryNets {
    pub(super) sheet_name: Option<String>,
    /// Nets whose owner is exactly this sheet/module.
    pub(super) nets_defined_here: BTreeSet<KiCadNetName>,
    /// Nets that must be declared as `io()` because they are owned by an ancestor.
    pub(super) nets_io_here: BTreeSet<KiCadNetName>,
}

/// Normalized KiCad sheetpath UUID chain (root is `/`, otherwise `/<uuid>/<uuid>/.../`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct KiCadSheetPath(String);

impl KiCadSheetPath {
    pub(super) fn root() -> Self {
        Self("/".to_string())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    pub(super) fn from_sheetpath_tstamps(sheetpath_tstamps: &str) -> Self {
        Self(normalize_sheetpath_tstamps(sheetpath_tstamps))
    }

    pub(super) fn depth(&self) -> usize {
        self.segments().count()
    }

    pub(super) fn segments(&self) -> impl Iterator<Item = &str> {
        self.0
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
    }

    pub(super) fn parent(&self) -> Option<Self> {
        let segments: Vec<&str> = self.segments().collect();
        if segments.is_empty() {
            return None;
        }
        if segments.len() == 1 {
            return Some(Self::root());
        }
        Some(Self(format!(
            "/{}/",
            segments[..segments.len() - 1].join("/")
        )))
    }

    pub(super) fn last_uuid(&self) -> Option<&str> {
        self.segments().last()
    }

    pub(super) fn is_ancestor_of(&self, other: &Self) -> bool {
        let a: Vec<&str> = self.segments().collect();
        let b: Vec<&str> = other.segments().collect();
        if a.len() > b.len() {
            return false;
        }
        a.iter().zip(b.iter()).all(|(x, y)| x == y)
    }
}

impl std::fmt::Display for KiCadSheetPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportSheetTree {
    /// Root schematic file path, relative to the KiCad project root when possible.
    pub(super) root_schematic: PathBuf,
    /// All sheet instance nodes keyed by sheetpath UUID chain.
    pub(super) nodes: BTreeMap<KiCadSheetPath, ImportSheetNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportSheetNode {
    /// UUID for this sheet instance (None for root).
    pub(super) sheet_uuid: Option<String>,
    /// Subsheet instance name ("Sheetname" property).
    pub(super) sheet_name: Option<String>,
    /// Schematic file referenced by this sheet instance ("Sheetfile" property), relative to project root if possible.
    pub(super) schematic_file: Option<PathBuf>,
    /// Child sheet instance paths.
    pub(super) children: BTreeSet<KiCadSheetPath>,
}

/// Key that can join KiCad schematic/netlist/PCB data for a single component instance.
///
/// This corresponds to:
/// - netlist: `(sheetpath (tstamps "..."))` + `(tstamps "...")`
/// - pcb: footprint `(path "/<sheet_uuid_chain>/<symbol_uuid>")`
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(super) struct KiCadUuidPathKey {
    /// Normalized to start and end with `/`. Root sheet is `/`.
    pub(super) sheetpath_tstamps: String,
    /// UUID string for the component/symbol instance.
    pub(super) symbol_uuid: String,
}

impl KiCadUuidPathKey {
    pub(super) fn pcb_path(&self) -> String {
        let sheetpath = normalize_sheetpath_tstamps(&self.sheetpath_tstamps);
        if sheetpath == "/" {
            format!("/{}", self.symbol_uuid)
        } else {
            format!("{sheetpath}{}", self.symbol_uuid)
        }
    }

    pub(super) fn from_pcb_path(pcb_path: &str) -> Result<Self> {
        let trimmed = pcb_path.trim();
        if !trimmed.starts_with('/') {
            anyhow::bail!("Expected KiCad PCB footprint path to start with '/': {pcb_path:?}");
        }
        let trimmed = trimmed.trim_end_matches('/');
        let mut parts: Vec<&str> = trimmed.split('/').filter(|s| !s.is_empty()).collect();
        let Some(symbol_uuid) = parts.pop() else {
            anyhow::bail!("KiCad PCB footprint path has no UUID segment: {pcb_path:?}");
        };
        let sheetpath_tstamps = if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}/", parts.join("/"))
        };
        Ok(Self {
            sheetpath_tstamps,
            symbol_uuid: symbol_uuid.to_string(),
        })
    }
}

impl std::fmt::Display for KiCadUuidPathKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.pcb_path())
    }
}

impl From<KiCadUuidPathKey> for String {
    fn from(value: KiCadUuidPathKey) -> Self {
        value.pcb_path()
    }
}

impl TryFrom<String> for KiCadUuidPathKey {
    type Error = anyhow::Error;

    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        KiCadUuidPathKey::from_pcb_path(&value)
    }
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportComponentData {
    pub(super) netlist: ImportNetlistComponent,
    pub(super) schematic: Option<ImportSchematicComponent>,
    pub(super) layout: Option<ImportLayoutComponent>,
}

impl ImportComponentData {
    pub(super) fn best_properties(&self) -> Option<&BTreeMap<String, String>> {
        if let Some(sch) = &self.schematic
            && let Some(unit) = sch.units.values().next()
        {
            return Some(&unit.properties);
        }

        self.layout.as_ref().map(|layout| &layout.properties)
    }
}

pub(super) fn find_property_ci<'a>(
    props: &'a BTreeMap<String, String>,
    keys: &[&str],
) -> Option<&'a str> {
    for want in keys {
        for (k, v) in props {
            if k.eq_ignore_ascii_case(want) {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed);
                }
            }
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportNetlistComponent {
    /// Refdes from the netlist export (human-facing; not used as primary identity).
    pub(super) refdes: KiCadRefDes,
    pub(super) value: Option<String>,
    pub(super) footprint: Option<String>,
    pub(super) sheetpath_names: Option<String>,
    /// KiCad PCB footprint `(path "...")` strings for every unit in a multi-unit symbol.
    ///
    /// For single-unit symbols, this has length 1.
    pub(super) unit_pcb_paths: Vec<KiCadUuidPathKey>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportSchematicComponent {
    /// Schematic symbol instances keyed by derived KiCad PCB footprint `(path "...")` strings.
    ///
    /// For single-unit symbols this has a single entry. Multi-unit symbols have one entry per unit.
    pub(super) units: BTreeMap<KiCadUuidPathKey, ImportSchematicUnit>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportSchematicUnit {
    /// KiCad schematic `(lib_name "...")` for symbols that have an instance-local embedded symbol.
    ///
    /// When present, prefer this as the key into the embedded `lib_symbols` table.
    pub(super) lib_name: Option<String>,
    pub(super) lib_id: Option<KiCadLibId>,
    pub(super) unit: Option<i64>,
    pub(super) at: Option<ImportSchematicAt>,
    pub(super) mirror: Option<String>,
    pub(super) in_bom: Option<bool>,
    pub(super) on_board: Option<bool>,
    pub(super) dnp: Option<bool>,
    pub(super) exclude_from_sim: Option<bool>,
    /// Raw `(instances ... (project ... (path "...")))` path string for debugging.
    pub(super) instance_path: Option<String>,
    /// All `(property "...")` name/value pairs on the symbol instance.
    pub(super) properties: BTreeMap<String, String>,
    /// Optional pin UUIDs keyed by pin number.
    pub(super) pins: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ImportSchematicAt {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) rot: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportLayoutComponent {
    pub(super) fpid: Option<String>,
    pub(super) uuid: Option<String>,
    pub(super) layer: Option<String>,
    pub(super) at: Option<ImportLayoutAt>,
    pub(super) sheetname: Option<String>,
    pub(super) sheetfile: Option<String>,
    pub(super) attrs: Vec<String>,
    pub(super) properties: BTreeMap<String, String>,
    pub(super) pads: BTreeMap<KiCadPinNumber, ImportLayoutPad>,
    #[serde(skip_serializing)]
    pub(super) footprint_sexpr: String,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportLayoutAt {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) rot: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportLayoutPad {
    pub(super) net_names: BTreeSet<KiCadNetName>,
    pub(super) uuids: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ImportNetData {
    /// The set of ports (component pin) connected to this net.
    ///
    /// The `component` field is the derived KiCad PCB footprint `(path "...")` string for the
    /// instance, allowing future joins against the PCB layout.
    pub(super) ports: BTreeSet<ImportNetPort>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct ImportNetPort {
    pub(super) component: KiCadUuidPathKey,
    pub(super) pin: KiCadPinNumber,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub(super) enum BoardNameSource {
    KicadProArgument,
}

#[derive(Debug, Default, Serialize, Clone)]
pub(super) struct KicadDiscoveredFiles {
    /// Paths are relative to `kicad_project_root`
    pub(super) kicad_pro: Vec<PathBuf>,
    pub(super) kicad_sch: Vec<PathBuf>,
    pub(super) kicad_pcb: Vec<PathBuf>,
    pub(super) kicad_sym: Vec<PathBuf>,
    pub(super) kicad_mod: Vec<PathBuf>,
    pub(super) kicad_prl: Vec<PathBuf>,
    pub(super) kicad_dru: Vec<PathBuf>,
    pub(super) fp_lib_table: Vec<PathBuf>,
    pub(super) sym_lib_table: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Clone)]
pub(super) struct ImportValidation {
    pub(super) selected: SelectedKicadFiles,
    pub(super) schematic_parity_ok: bool,
    pub(super) schematic_parity_violations: usize,
    pub(super) schematic_parity_tolerated: usize,
    pub(super) schematic_parity_blocking: usize,
    pub(super) erc_errors: usize,
    pub(super) erc_warnings: usize,
    pub(super) drc_errors: usize,
    pub(super) drc_warnings: usize,
}

#[derive(Debug, Serialize, Clone)]
pub(super) struct GeneratedArtifacts {
    pub(super) board_dir: PathBuf,
    pub(super) board_zen: PathBuf,
    pub(super) validation_diagnostics_json: PathBuf,
    pub(super) import_extraction_json: PathBuf,
    pub(super) layout_dir: PathBuf,
    pub(super) layout_kicad_pro: PathBuf,
    pub(super) layout_kicad_pcb: PathBuf,
    pub(super) portable_kicad_project_zip: PathBuf,
}

pub(super) struct ImportValidationRun {
    pub(super) summary: ImportValidation,
    pub(super) diagnostics: Diagnostics,
}

#[derive(Debug, Serialize, Clone)]
pub(super) struct SelectedKicadFiles {
    /// Relative to `kicad_project_root`
    pub(super) kicad_pro: PathBuf,
    /// Relative to `kicad_project_root`
    pub(super) kicad_sch: PathBuf,
    /// Relative to `kicad_project_root`
    pub(super) kicad_pcb: PathBuf,
}

pub(super) fn normalize_sheetpath_tstamps(sheetpath: &str) -> String {
    let trimmed = sheetpath.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }
    let without = trimmed.trim_matches('/');
    format!("/{without}/")
}

pub(super) fn rel_to_root(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

#[cfg(test)]
mod sheet_path_tests {
    use super::KiCadSheetPath;

    #[test]
    fn sheet_path_parent_and_depth() {
        let root = KiCadSheetPath::root();
        assert_eq!(root.depth(), 0);
        assert_eq!(root.parent(), None);

        let one = KiCadSheetPath::from_sheetpath_tstamps("/a/");
        assert_eq!(one.depth(), 1);
        assert_eq!(one.parent().unwrap().as_str(), "/");

        let two = KiCadSheetPath::from_sheetpath_tstamps("/a/b/");
        assert_eq!(two.depth(), 2);
        assert_eq!(two.parent().unwrap().as_str(), "/a/");
        assert_eq!(two.last_uuid().unwrap(), "b");
    }
}
