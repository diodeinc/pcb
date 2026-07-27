use super::*;
use anyhow::{Context, Result, bail};
use pcb_sch::{AttributeValue, InstanceKind, Schematic};
use pcb_zen_core::lang::eval::EvalOutput;
use starlark::collections::SmallMap;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(super) struct RegistryReusePlan {
    pub(super) module_path: String,
    pub(super) component_name: String,
    pub(super) io_pins: BTreeMap<String, BTreeSet<KiCadPinNumber>>,
    pub(super) staged_entrypoint: PathBuf,
}

#[derive(Clone)]
struct EvaluatedCandidate {
    component_name: String,
    io_pins: BTreeMap<String, BTreeSet<KiCadPinNumber>>,
    part_mpn: Option<String>,
    part_manufacturer: Option<String>,
    footprint: Option<String>,
}

/// Run-scoped context for registry-module reuse: the staged board it substitutes into, the workspace
/// resolution used to evaluate candidates, and the per-entrypoint evaluation memo.
///
/// Candidates are evaluated in the *importer's* workspace, resolved once, rather than each in its own. A
/// cached registry package carries an empty `pcb.toml`, so resolving from the package would make it its
/// own workspace root and materialize a full stdlib copy into the shared cache for every candidate —
/// permanent growth in `~/.pcb/cache` caused by a read-only compatibility check.
///
/// Evaluation is memoized per entrypoint, failures included: one module is reachable through several
/// candidate records, through MPN spellings that normalize equal, and through part groups sharing an MPN.
pub(super) struct RegistryReuseContext {
    board_dir: PathBuf,
    /// Built on first use so an import with no cached candidates resolves nothing.
    eval_state: Option<crate::build::BuildEvalState>,
    /// `Err` holds the rendered rejection reason: a candidate that failed to evaluate must be
    /// rejected the same way every time it is reached, and the reason is logged at each site.
    evaluated: HashMap<PathBuf, Result<EvaluatedCandidate, String>>,
}

impl RegistryReuseContext {
    pub(super) fn new(board_dir: &Path) -> Self {
        Self {
            board_dir: board_dir.to_path_buf(),
            eval_state: None,
            evaluated: HashMap::new(),
        }
    }

    fn evaluate(&mut self, entrypoint: &Path) -> Result<EvaluatedCandidate> {
        if let Some(cached) = self.evaluated.get(entrypoint) {
            return cached.clone().map_err(anyhow::Error::msg);
        }

        if self.eval_state.is_none() {
            self.eval_state = Some(offline_eval_state(&self.board_dir).context(
                "Failed to prepare the board workspace for registry candidate evaluation",
            )?);
        }
        let eval_state = self
            .eval_state
            .as_ref()
            .expect("eval state was just initialized");

        let stored =
            evaluate_candidate(eval_state, entrypoint).map_err(|error| format!("{error:#}"));
        self.evaluated
            .insert(entrypoint.to_path_buf(), stored.clone());
        stored.map_err(anyhow::Error::msg)
    }
}

/// The outcome of planning a registry-module substitution for one imported component.
pub(super) enum RegistryReuseOutcome {
    /// Exactly one cached entrypoint was compatible.
    Reused(RegistryReusePlan),
    /// Two or more cached entrypoints were compatible, so reuse fell back to a generated component.
    /// The count is carried so a consumer can tell this apart from `NoMatch`: which entrypoints are
    /// cached locally varies per machine and is not something the source design states.
    Ambiguous(usize),
    /// Nothing usable matched. Every other fallback lands here.
    NoMatch,
}

/// What one imported component needs matched against the cached registry.
pub(super) struct RegistryReuseRequest<'a> {
    pub(super) component: &'a ImportComponentData,
    pub(super) instance_anchors: &'a [KiCadUuidPathKey],
    pub(super) components: &'a BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    pub(super) port_to_net: &'a BTreeMap<ImportNetPort, KiCadNetName>,
    pub(super) expected_pins: &'a BTreeSet<KiCadPinNumber>,
    pub(super) lookup: &'a ImportRegistryMpnLookup,
}

/// Plans a registry-module substitution for one imported component.
pub(super) fn try_reuse_registry_component(
    request: RegistryReuseRequest<'_>,
    context: &mut RegistryReuseContext,
    writer: &mut output::ImportWriter,
) -> Result<RegistryReuseOutcome> {
    let RegistryReuseRequest {
        component,
        instance_anchors,
        components,
        port_to_net,
        expected_pins,
        lookup,
    } = request;
    let board_dir = context.board_dir.clone();
    let board_dir = board_dir.as_path();
    let Some(source_mpn) = registry_lookup::explicit_mpn(component) else {
        return Ok(RegistryReuseOutcome::NoMatch);
    };
    let normalized = pcb_diode_api::normalize_mpn_for_lookup(source_mpn);
    let candidates = lookup
        .candidates_by_mpn
        .iter()
        .filter(|(mpn, _)| pcb_diode_api::normalize_mpn_for_lookup(mpn) == normalized)
        .flat_map(|(_, candidates)| candidates)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(RegistryReuseOutcome::NoMatch);
    }

    let expected_footprint = component
        .netlist
        .footprint
        .as_deref()
        .map(pcb_sexpr::board::footprint_name_from_fpid);
    let source_manufacturer = registry_lookup::explicit_manufacturer(component);
    // MPNs are not unique across manufacturers. When the source names no manufacturer, the only
    // available evidence is the cached exact-MPN records, and it proves identity solely when every
    // record that names a manufacturer agrees. Grouping by folding key makes spelling variants of
    // one manufacturer count once; only the count and `manufacturers_match` comparisons read this
    // set, so it needs no display spelling.
    let indexed_manufacturer_keys = candidates
        .iter()
        .map(|candidate| registry_lookup::manufacturer_key(&candidate.manufacturer))
        .filter(|key| !key.is_empty())
        .collect::<BTreeSet<_>>();
    // The source component's own footprint geometry, when import resolved any. Its presence decides
    // how footprint identity is judged below: by land pattern if it is here, by name if it is not.
    let source_footprint = footprint_identity::component_footprint_source(component);
    let mut compatible = Vec::new();
    for candidate in candidates {
        // The indexed footprint is only a *name*, so it can pre-filter nothing when the real
        // comparison is geometric — the same u-blox module is `ublox_SAM-M8Q` in one library and
        // `SAM-M10Q-00B` in another. Without source geometry the name is the only evidence there is,
        // so it stays a cheap gate for that case.
        if source_footprint.is_none()
            && !candidate_footprint_matches(
                Some(&candidate.footprint),
                expected_footprint.as_deref(),
            )
        {
            log::debug!(
                "Rejected registry candidate {}@{}: indexed footprint name differs from source and the source geometry is unavailable",
                candidate.module_url,
                candidate.module_version
            );
            continue;
        }
        let Some(package_root) = cached_candidate_package_root(board_dir, candidate)? else {
            continue;
        };
        for entrypoint_url in &candidate.entrypoints {
            let Some(entrypoint_rel) = entrypoint_relative_path(candidate, entrypoint_url) else {
                continue;
            };
            let entrypoint = package_root.join(&entrypoint_rel);
            if !entrypoint.is_file() || fs::symlink_metadata(&entrypoint)?.file_type().is_symlink()
            {
                continue;
            }
            let evaluated = match context.evaluate(&entrypoint) {
                Ok(evaluated) => evaluated,
                Err(error) => {
                    log::debug!(
                        "Rejected registry candidate {}: {error:#}",
                        entrypoint.display()
                    );
                    continue;
                }
            };
            if evaluated
                .io_pins
                .values()
                .flatten()
                .cloned()
                .collect::<BTreeSet<_>>()
                != *expected_pins
            {
                log::debug!(
                    "Rejected registry candidate {}: physical pad set differs from imported symbol",
                    entrypoint.display()
                );
                continue;
            }
            if !pin_groups_match_every_instance(
                &evaluated.io_pins,
                instance_anchors,
                components,
                port_to_net,
            )? {
                log::debug!(
                    "Rejected registry candidate {}: one IO groups source pins with different connectivity",
                    entrypoint.display()
                );
                continue;
            }
            let Some(part_mpn) = evaluated.part_mpn.as_deref() else {
                log::debug!(
                    "Rejected registry candidate {}: component has no Part metadata",
                    entrypoint.display()
                );
                continue;
            };
            if pcb_diode_api::normalize_mpn_for_lookup(part_mpn) != normalized {
                log::debug!(
                    "Rejected registry candidate {}: evaluated Part MPN differs from source",
                    entrypoint.display()
                );
                continue;
            }
            let part_manufacturer = evaluated.part_manufacturer.as_deref();
            match source_manufacturer {
                Some(source_manufacturer) => {
                    if !part_manufacturer.is_some_and(|part_manufacturer| {
                        registry_lookup::manufacturers_match(part_manufacturer, source_manufacturer)
                    }) {
                        log::debug!(
                            "Rejected registry candidate {}: evaluated Part manufacturer differs from source",
                            entrypoint.display()
                        );
                        continue;
                    }
                }
                None => match indexed_manufacturer_keys.len() {
                    0 => {}
                    1 => {
                        let indexed_key = indexed_manufacturer_keys.iter().next().unwrap();
                        if part_manufacturer.is_some_and(|part_manufacturer| {
                            !registry_lookup::manufacturers_match(part_manufacturer, indexed_key)
                        }) {
                            log::debug!(
                                "Rejected registry candidate {}: evaluated Part manufacturer differs from the single indexed manufacturer for this MPN",
                                entrypoint.display()
                            );
                            continue;
                        }
                    }
                    _ => {
                        log::debug!(
                            "Rejected registry candidate {}: exact MPN is indexed under multiple manufacturers and the source names none",
                            entrypoint.display()
                        );
                        continue;
                    }
                },
            }
            if let Err(error) = validate_registry_package_tree(&package_root) {
                log::debug!(
                    "Rejected registry candidate {}: package cannot be copied safely: {error:#}",
                    entrypoint.display()
                );
                continue;
            }
            match candidate_footprint_is_compatible(
                &package_root,
                evaluated.footprint.as_deref(),
                source_footprint.as_deref(),
                expected_footprint.as_deref(),
                expected_pins,
            ) {
                Ok(true) => {}
                Ok(false) => {
                    log::debug!(
                        "Rejected registry candidate {}: footprint is not an acceptable substitute for the source",
                        entrypoint.display()
                    );
                    continue;
                }
                Err(error) => {
                    log::debug!(
                        "Rejected registry candidate {}: failed to inspect footprint geometry: {error:#}",
                        entrypoint.display()
                    );
                    continue;
                }
            }
            // One entry per distinct entrypoint file. The same module is reachable through several
            // candidate records — two symbols of one module that share an MPN, or the same module
            // present in two cached indexes — and every record carries that module's whole entrypoint
            // list. Counting those repeats made one usable entrypoint look like competing matches, so
            // a valid reuse was reported ambiguous and fell back to a generated component.
            if compatible
                .iter()
                .any(|(_, root, seen, _)| root == &package_root && seen == &entrypoint_rel)
            {
                continue;
            }
            compatible.push((candidate, package_root.clone(), entrypoint_rel, evaluated));
        }
    }

    match compatible.len() {
        0 => Ok(RegistryReuseOutcome::NoMatch),
        1 => {
            let (candidate, package_root, entrypoint_rel, evaluated) = compatible.pop().unwrap();
            let key = format!(
                "{}@{}:{}",
                candidate.module_url,
                candidate.module_version,
                entrypoint_rel.display()
            );
            let hash = Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes())
                .simple()
                .to_string();
            let package_name = package_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("component");
            let destination_rel = PathBuf::from("components")
                .join("registry")
                .join(&hash[..16])
                .join(package_name);
            let destination = board_dir.join(&destination_rel);
            // Copied unconditionally, not only when the destination is absent. The writer decides per
            // file, so an unchanged package is a silent no-op, a stale one is refreshed under
            // `--force`, and a differing one is kept and reported like any other output. Skipping the
            // copy whenever the directory existed meant `--force` could never refresh a stale or
            // damaged package left by an earlier import.
            let existed = destination.exists();
            if let Err(error) =
                writer.copy_tree(&package_root, &destination, &should_skip_registry_path)
            {
                // Only clean up a directory this run created: import must never delete output an
                // earlier run produced and this run kept.
                if !existed {
                    let _ = fs::remove_dir_all(&destination);
                }
                log::debug!(
                    "Rejected registry candidate {}: failed to materialize package: {error:#}",
                    entrypoint_rel.display()
                );
                return Ok(RegistryReuseOutcome::NoMatch);
            }
            if !destination.is_dir() || fs::symlink_metadata(&destination)?.file_type().is_symlink()
            {
                log::debug!(
                    "Rejected registry candidate {}: destination is not a regular directory",
                    destination.display()
                );
                return Ok(RegistryReuseOutcome::NoMatch);
            }
            let staged_entrypoint = destination_rel.join(&entrypoint_rel);
            let module_path = path_to_posix(&staged_entrypoint);
            Ok(RegistryReuseOutcome::Reused(RegistryReusePlan {
                module_path,
                component_name: evaluated.component_name,
                io_pins: evaluated.io_pins,
                staged_entrypoint,
            }))
        }
        count => {
            // Ambiguity is not actionable for the user: there is no mapping input, and which
            // entrypoints are cached locally varies per machine. Fall back for this component only,
            // exactly as zero compatible candidates does.
            log::debug!(
                "Ambiguous cached registry entrypoints for exact MPN {source_mpn}: {}",
                compatible
                    .iter()
                    .map(|(candidate, _, entrypoint_rel, _)| format!(
                        "{}@{}:{}",
                        candidate.module_url,
                        candidate.module_version,
                        entrypoint_rel.display()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            eprintln!(
                "{} (MPN {source_mpn}) matched {count} electrically compatible cached registry entrypoints; using generated fallback Zener",
                component.netlist.refdes
            );
            // The stderr line above is for the human running the import; the returned count is the
            // same fact for the machine-readable report.
            Ok(RegistryReuseOutcome::Ambiguous(count))
        }
    }
}

fn pin_groups_match_every_instance(
    io_pins: &BTreeMap<String, BTreeSet<KiCadPinNumber>>,
    instance_anchors: &[KiCadUuidPathKey],
    components: &BTreeMap<KiCadUuidPathKey, ImportComponentData>,
    port_to_net: &BTreeMap<ImportNetPort, KiCadNetName>,
) -> Result<bool> {
    for anchor in instance_anchors {
        let component = components
            .get(anchor)
            .context("Registry candidate instance is missing from import data")?;
        for pins in io_pins.values() {
            let resolved = pins
                .iter()
                .map(|pin| generate::resolve_instance_pin_net(anchor, component, pin, port_to_net))
                .collect::<Result<Vec<_>>>()?;
            let connected = resolved.iter().flatten().collect::<BTreeSet<_>>();
            if connected.len() > 1
                || (pins.len() > 1 && !connected.is_empty() && resolved.iter().any(Option::is_none))
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn cached_candidate_package_root(
    board_dir: &Path,
    candidate: &ImportRegistryMpnCandidate,
) -> Result<Option<PathBuf>> {
    let relative = PathBuf::from(&candidate.module_url).join(&candidate.module_version);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Ok(None);
    }

    let mut bases = vec![board_dir.join("vendor")];
    if let Some(home) = dirs::home_dir() {
        bases.push(home.join(".pcb").join("cache"));
    }
    for base in bases {
        let Ok(base_metadata) = fs::symlink_metadata(&base) else {
            continue;
        };
        if !base_metadata.is_dir() || base_metadata.file_type().is_symlink() {
            continue;
        }
        let mut root = base;
        let mut valid = true;
        for component in relative.components() {
            let std::path::Component::Normal(segment) = component else {
                valid = false;
                break;
            };
            root.push(segment);
            let Ok(metadata) = fs::symlink_metadata(&root) else {
                valid = false;
                break;
            };
            if metadata.file_type().is_symlink() {
                valid = false;
                break;
            }
        }
        if valid && root.is_dir() {
            return Ok(Some(root));
        }
    }
    Ok(None)
}

fn entrypoint_relative_path(
    candidate: &ImportRegistryMpnCandidate,
    entrypoint_url: &str,
) -> Option<PathBuf> {
    let relative = entrypoint_url
        .strip_prefix(&candidate.module_url)?
        .trim_start_matches('/');
    let path = PathBuf::from(relative);
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    Some(path)
}

fn evaluate_candidate(
    eval_state: &crate::build::BuildEvalState,
    entrypoint: &Path,
) -> Result<EvaluatedCandidate> {
    let (eval_output, schematic) = evaluate_candidate_output(eval_state, entrypoint)?;
    let (component_ref, component) = only_component(&schematic)?;

    if eval_output
        .signature
        .iter()
        .any(|parameter| parameter.is_config() && parameter.required)
    {
        bail!("Candidate has required configuration without a default");
    }
    let io_names = eval_output
        .signature
        .iter()
        .filter(|parameter| !parameter.is_config())
        .map(|parameter| parameter.name.clone())
        .collect::<BTreeSet<_>>();
    if io_names.is_empty() {
        bail!("Candidate has no module IO");
    }

    let pads_by_port = component
        .children
        .values()
        .filter_map(|port_ref| {
            let port = schematic.instances.get(port_ref)?;
            let AttributeValue::Array(pads) = port.attributes.get("pads")? else {
                return None;
            };
            Some((
                port_ref,
                pads.iter()
                    .filter_map(AttributeValue::string)
                    .map(|pad| KiCadPinNumber::from(pad.to_string()))
                    .collect::<BTreeSet<_>>(),
            ))
        })
        .collect::<HashMap<_, _>>();

    let mut io_pins = BTreeMap::new();
    for io_name in io_names {
        let net = schematic
            .nets
            .get(&io_name)
            .with_context(|| format!("Candidate IO {io_name} has no root net"))?;
        let pads = net
            .ports
            .iter()
            .filter(|port| port.instance_path.starts_with(&component_ref.instance_path))
            .filter_map(|port| pads_by_port.get(port))
            .flat_map(|pads| pads.iter().cloned())
            .collect::<BTreeSet<_>>();
        if pads.is_empty() {
            bail!("Candidate IO {io_name} is not connected to a physical component pad");
        }
        io_pins.insert(io_name, pads);
    }

    Ok(EvaluatedCandidate {
        component_name: component.type_ref.module_name.to_string(),
        io_pins,
        part_mpn: component.part().map(|part| part.mpn.clone()),
        part_manufacturer: component
            .part()
            .map(|part| part.manufacturer.trim().to_string())
            .filter(|manufacturer| !manufacturer.is_empty()),
        footprint: component.string_attr(&["footprint"]),
    })
}

fn evaluate_candidate_output(
    eval_state: &crate::build::BuildEvalState,
    entrypoint: &Path,
) -> Result<(EvalOutput, Schematic)> {
    let mut has_errors = false;
    let mut has_warnings = false;
    let suppress = vec![
        "bom.unspecified".to_string(),
        "bom.underspecified".to_string(),
    ];
    let passes: Vec<Box<dyn pcb_zen_core::DiagnosticsPass>> =
        vec![Box::new(pcb_zen_core::SuppressPass::new(suppress))];
    let result = eval_state.build(
        entrypoint,
        SmallMap::new(),
        passes,
        false,
        &mut has_errors,
        &mut has_warnings,
    );
    if has_errors {
        bail!("Candidate evaluation produced errors");
    }
    let output = result
        .eval_output
        .context("Candidate evaluation produced no output")?;
    let schematic = result
        .schematic
        .context("Candidate evaluation produced no schematic")?;
    Ok((output, schematic))
}

fn only_component(schematic: &Schematic) -> Result<(&pcb_sch::InstanceRef, &pcb_sch::Instance)> {
    let components = schematic
        .instances
        .iter()
        .filter(|(_, instance)| instance.kind == InstanceKind::Component)
        .collect::<Vec<_>>();
    match components.as_slice() {
        [component] => Ok(*component),
        _ => bail!(
            "Registry entrypoint must contain exactly one physical component, found {}",
            components.len()
        ),
    }
}

/// The bare footprint name a reference denotes: its last path or library-nickname segment, with the
/// `.kicad_mod` extension removed.
///
/// Footprint identity is compared on this name in three places, so the two-step strip lives in one.
pub(super) fn footprint_basename(footprint: &str) -> &str {
    let filename = footprint.rsplit(['/', ':']).next().unwrap_or(footprint);
    filename.strip_suffix(".kicad_mod").unwrap_or(filename)
}

fn candidate_footprint_matches(footprint: Option<&str>, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let Some(footprint) = footprint else {
        return false;
    };
    footprint_basename(footprint) == expected
}

/// Whether the candidate's footprint is an acceptable substitute for the source component's.
///
/// Physical identity is preferred and footprint *names* are then ignored entirely: a name proves
/// nothing either way, since one library calls the u-blox SAM-M10Q land pattern `ublox_SAM-M8Q` and
/// another calls it `SAM-M10Q-00B`, while two unrelated parts can share a name across libraries.
///
/// When import could not resolve the source geometry — a bundled-stdlib reference, or a footprint the
/// design names but no library provides — there is nothing to compare, so this falls back to the older
/// and weaker evidence: equal footprint names plus an identical numbered-pad set.
fn candidate_footprint_is_compatible(
    package_root: &Path,
    candidate_footprint: Option<&str>,
    source_footprint: Option<&str>,
    expected_footprint_name: Option<&str>,
    expected_pins: &BTreeSet<KiCadPinNumber>,
) -> Result<bool> {
    let Some(source) = source_footprint else {
        return Ok(
            candidate_footprint_matches(candidate_footprint, expected_footprint_name)
                && candidate_footprint_pads_match(
                    package_root,
                    candidate_footprint,
                    expected_pins,
                )?,
        );
    };
    let Some(candidate) = locate_candidate_footprint(package_root, candidate_footprint)? else {
        return Ok(false);
    };
    footprint_identity::same_land_pattern(source, &candidate)
}

/// Read the candidate package's own `.kicad_mod` for `footprint`.
///
/// `None` when the geometry is absent or its basename is ambiguous inside the package: a substitution
/// must not rest on a guess about which of two same-named files the candidate meant.
fn locate_candidate_footprint(
    package_root: &Path,
    footprint: Option<&str>,
) -> Result<Option<String>> {
    let Some(footprint) = footprint else {
        return Ok(None);
    };
    if !footprint.ends_with(".kicad_mod") || Path::new(footprint).is_absolute() {
        return Ok(None);
    }
    let filename = format!("{}.kicad_mod", footprint_basename(footprint));
    let mut matches = Vec::new();
    find_footprint_files(package_root, &filename, &mut matches)?;
    if matches.len() != 1 {
        return Ok(None);
    }
    let source = fs::read_to_string(&matches[0])?;
    if pcb_sexpr::kicad::footprint::validate_footprint_source(&source).is_err() {
        return Ok(None);
    }
    Ok(Some(source))
}

/// Whether the candidate footprint's numbered-pad set is exactly the source component's.
///
/// The weaker of the two footprint checks, used only when the source geometry is unavailable and the
/// pad *numbers* are all that can be compared.
fn candidate_footprint_pads_match(
    package_root: &Path,
    footprint: Option<&str>,
    expected_pins: &BTreeSet<KiCadPinNumber>,
) -> Result<bool> {
    let Some(source) = locate_candidate_footprint(package_root, footprint)? else {
        return Ok(false);
    };
    let Ok(lands) = footprint_identity::pad_lands(&source) else {
        return Ok(false);
    };
    Ok(lands.keys().cloned().collect::<BTreeSet<_>>() == *expected_pins)
}

/// Directories that never hold a package's *own* footprint geometry.
///
/// Deliberately not [`should_skip_registry_path`], which answers a different question — what is safe
/// to copy. `vendor/` holds a package's dependencies, so a footprint found there belongs to another
/// package and must not make this one's basename look ambiguous.
const FOOTPRINT_SEARCH_SKIP_DIRS: [&str; 4] = [".git", ".pcb", "target", "vendor"];

/// Every file named `filename` anywhere in the package. A symlink is refused rather than followed: it
/// could resolve outside the package entirely.
fn find_footprint_files(current: &Path, filename: &str, matches: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            bail!("registry package contains symlink {}", path.display());
        }
        if metadata.is_dir() {
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| FOOTPRINT_SEARCH_SKIP_DIRS.contains(&name))
            {
                continue;
            }
            find_footprint_files(&path, filename, matches)?;
        } else if metadata.is_file() && entry.file_name() == std::ffi::OsStr::new(filename) {
            matches.push(path);
        }
    }
    Ok(())
}

fn should_skip_registry_path(relative: &Path) -> bool {
    relative == Path::new("pcb.toml")
        || relative.extension().is_some_and(|ext| ext == "lock")
        || relative.components().next().is_some_and(|component| {
            matches!(component, std::path::Component::Normal(name) if name == ".pcb" || name == ".git" || name == "target")
        })
}

fn validate_registry_package_tree(root: &Path) -> Result<()> {
    fn visit(current: &Path, root: &Path) -> Result<()> {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap();
            if should_skip_registry_path(relative) {
                continue;
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "Registry package contains unsupported symlink: {}",
                    path.display()
                );
            }
            if metadata.is_dir() {
                visit(&path, root)?;
            }
        }
        Ok(())
    }
    visit(root, root)
}

fn path_to_posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_candidate_io_that_groups_different_source_nets() {
        let anchor = KiCadUuidPathKey::from_pcb_path("/part").unwrap();
        let component = ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: KiCadRefDes::from("U1".to_string()),
                value: None,
                footprint: None,
                sheetpath_names: None,
                unit_pcb_paths: vec![anchor.clone()],
            },
            schematic: None,
            layout: None,
        };
        let components = BTreeMap::from([(anchor.clone(), component)]);
        let ports = BTreeMap::from([
            (
                ImportNetPort {
                    component: anchor.clone(),
                    pin: KiCadPinNumber::from("1".to_string()),
                },
                KiCadNetName::from("A".to_string()),
            ),
            (
                ImportNetPort {
                    component: anchor.clone(),
                    pin: KiCadPinNumber::from("2".to_string()),
                },
                KiCadNetName::from("B".to_string()),
            ),
        ]);
        let io_pins = BTreeMap::from([(
            "GND".to_string(),
            BTreeSet::from([
                KiCadPinNumber::from("1".to_string()),
                KiCadPinNumber::from("2".to_string()),
            ]),
        )]);

        assert!(
            !pin_groups_match_every_instance(&io_pins, &[anchor], &components, &ports).unwrap()
        );
    }

    const MODULE_URL: &str = "example.com/registry/components/Acme/ABC-1";
    const MODULE_VERSION: &str = "1.0.0";

    /// A vendored entrypoint that is compatible with the source component built by
    /// [`source_component`]: same MPN, same manufacturer, same footprint, one pad numbered `1`.
    const COMPATIBLE_ZEN: &str = r#"P1 = io(Net)
Component(
    name="ABC_1",
    footprint="TEST.kicad_mod",
    pin_defs={"P1": "1"},
    pins={"P1": P1},
    mpn="ABC-1",
    manufacturer="Acme",
)
"#;

    /// A footprint whose only numbered pad is `1`, matching the expected pad set used by the
    /// fixture helpers below.
    fn one_pad_footprint(name: &str) -> String {
        format!(
            r#"(footprint "{name}" (version 20240108) (generator pcbnew) (layer "F.Cu")
  (pad "1" thru_hole circle (at 0 0) (size 1 1) (drill 0.5) (layers "*.Cu" "*.Mask")))"#
        )
    }

    /// Board directory containing one vendored registry package. Every gate in
    /// `try_reuse_registry_component` is satisfied by default so each test can break exactly one.
    struct ReuseFixture {
        _temp: tempfile::TempDir,
        board: PathBuf,
        package: PathBuf,
    }

    impl ReuseFixture {
        fn new(entrypoints: &[(&str, &str)]) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let board = temp.path().join("board");
            fs::create_dir(&board).unwrap();
            crate::new::init_board_repo(&board, "Board", "").unwrap();

            let package = board.join("vendor").join(MODULE_URL).join(MODULE_VERSION);
            fs::create_dir_all(&package).unwrap();
            for (name, body) in entrypoints {
                fs::write(package.join(name), body).unwrap();
            }
            fs::write(package.join("TEST.kicad_mod"), one_pad_footprint("TEST")).unwrap();

            Self {
                _temp: temp,
                board,
                package,
            }
        }

        /// Fixture with the single default-compatible entrypoint `ABC-1.zen`.
        fn compatible() -> Self {
            Self::new(&[("ABC-1.zen", COMPATIBLE_ZEN)])
        }

        fn entrypoint(&self, name: &str) -> PathBuf {
            self.package.join(name)
        }

        fn run(
            &self,
            component: &ImportComponentData,
            candidates: Vec<ImportRegistryMpnCandidate>,
        ) -> Result<Option<RegistryReusePlan>> {
            Ok(match self.run_outcome(component, candidates)? {
                RegistryReuseOutcome::Reused(plan) => Some(plan),
                _ => None,
            })
        }

        /// `run`, keeping the outcome so a test can assert on the ambiguity count.
        fn run_outcome(
            &self,
            component: &ImportComponentData,
            candidates: Vec<ImportRegistryMpnCandidate>,
        ) -> Result<RegistryReuseOutcome> {
            self.run_outcome_with_force(component, candidates, false)
        }

        fn run_outcome_with_force(
            &self,
            component: &ImportComponentData,
            candidates: Vec<ImportRegistryMpnCandidate>,
            force: bool,
        ) -> Result<RegistryReuseOutcome> {
            let lookup = ImportRegistryMpnLookup {
                cached_index_available: true,
                queried_mpns: BTreeSet::from(["ABC-1".to_string()]),
                candidates_by_mpn: BTreeMap::from([("ABC-1".to_string(), candidates)]),
                lookup_error: None,
            };
            let anchor = component.netlist.unit_pcb_paths[0].clone();
            let components = BTreeMap::from([(anchor.clone(), component.clone())]);
            let port_to_net = BTreeMap::from([(
                ImportNetPort {
                    component: anchor.clone(),
                    pin: KiCadPinNumber::from("1".to_string()),
                },
                KiCadNetName::from("NET".to_string()),
            )]);
            try_reuse_registry_component(
                RegistryReuseRequest {
                    component,
                    instance_anchors: &[anchor],
                    components: &components,
                    port_to_net: &port_to_net,
                    expected_pins: &BTreeSet::from([KiCadPinNumber::from("1".to_string())]),
                    lookup: &lookup,
                },
                &mut RegistryReuseContext::new(&self.board),
                &mut output::ImportWriter::new(&self.board, "Board", force)?,
            )
        }

        /// Evaluate one entrypoint the way an import would, through a context rooted at the staged
        /// board rather than at the candidate's own directory.
        fn evaluate(&self, entrypoint: &Path) -> Result<EvaluatedCandidate> {
            RegistryReuseContext::new(&self.board).evaluate(entrypoint)
        }

        /// Convenience for the common single-candidate case.
        fn run_default(
            &self,
            component: &ImportComponentData,
            entrypoints: &[&str],
        ) -> Result<Option<RegistryReusePlan>> {
            self.run(component, vec![candidate(entrypoints, "Acme")])
        }
    }

    /// Imported KiCad component with MPN `ABC-1` and footprint id `Local:TEST`.
    fn source_component(manufacturer: Option<&str>) -> ImportComponentData {
        let anchor = KiCadUuidPathKey::from_pcb_path("/part").unwrap();
        let mut properties =
            BTreeMap::from([("Manufacturer_Part_Number".to_string(), "ABC-1".to_string())]);
        if let Some(manufacturer) = manufacturer {
            properties.insert("Manufacturer".to_string(), manufacturer.to_string());
        }
        ImportComponentData {
            netlist: ImportNetlistComponent {
                refdes: KiCadRefDes::from("U1".to_string()),
                value: Some("ABC-1".to_string()),
                footprint: Some("Local:TEST".to_string()),
                sheetpath_names: None,
                unit_pcb_paths: vec![anchor.clone()],
            },
            schematic: Some(ImportSchematicComponent {
                units: BTreeMap::from([(
                    anchor,
                    ImportSchematicUnit {
                        lib_name: None,
                        lib_id: None,
                        unit: Some(1),
                        at: None,
                        mirror: None,
                        in_bom: Some(true),
                        on_board: Some(true),
                        dnp: Some(false),
                        exclude_from_sim: Some(false),
                        instance_path: None,
                        properties,
                        pins: None,
                    },
                )]),
            }),
            layout: None,
        }
    }

    /// Give a source component resolved footprint geometry, which is what switches footprint identity
    /// from name comparison to land-pattern comparison.
    fn with_footprint_geometry(
        mut component: ImportComponentData,
        fpid: &str,
        source: &str,
    ) -> ImportComponentData {
        component.netlist.footprint = Some(fpid.to_string());
        component.layout = Some(ImportLayoutComponent {
            fpid: Some(fpid.to_string()),
            unresolved_footprint: None,
            uuid: None,
            layer: None,
            at: None,
            sheetname: None,
            sheetfile: None,
            attrs: Vec::new(),
            properties: BTreeMap::new(),
            pads: BTreeMap::new(),
            footprint_geometry: ImportFootprintGeometry::LibraryFile(source.to_string()),
        });
        component
    }

    /// `Result::unwrap_err` requires `Debug` on the success type; these results carry values that
    /// deliberately do not implement it.
    fn error_message<T>(result: Result<T>) -> String {
        match result {
            Ok(_) => panic!("expected an error"),
            Err(error) => format!("{error:#}"),
        }
    }

    fn candidate(entrypoints: &[&str], manufacturer: &str) -> ImportRegistryMpnCandidate {
        ImportRegistryMpnCandidate {
            registry_id: "registry".to_string(),
            registry_mpn: "ABC-1".to_string(),
            manufacturer: manufacturer.to_string(),
            footprint: "TEST".to_string(),
            module_url: MODULE_URL.to_string(),
            module_version: MODULE_VERSION.to_string(),
            entrypoints: entrypoints
                .iter()
                .map(|name| format!("{MODULE_URL}/{name}"))
                .collect(),
            symbol_preferred: false,
            module_preferred: false,
        }
    }

    #[test]
    fn selects_and_materializes_single_compatible_vendored_entrypoint() {
        let fixture = ReuseFixture::compatible();
        let component = source_component(None);
        let plan = fixture
            .run_default(&component, &["ABC-1.zen"])
            .unwrap()
            .unwrap();

        assert_eq!(plan.component_name, "ABC_1");
        assert_eq!(
            plan.io_pins,
            BTreeMap::from([(
                "P1".to_string(),
                BTreeSet::from([KiCadPinNumber::from("1".to_string())]),
            )])
        );
        assert!(fixture.board.join(&plan.staged_entrypoint).is_file());
        let copied = fixture
            .evaluate(&fixture.board.join(&plan.staged_entrypoint))
            .unwrap();
        assert_eq!(copied.part_mpn.as_deref(), Some("ABC-1"));
        assert!(plan.module_path.starts_with("components/registry/"));
        assert!(!plan.module_path.contains(".pcb/cache"));
    }

    /// A real cached registry package ships its own empty `pcb.toml`, which makes it its own
    /// workspace root when resolution starts from inside it. Evaluating a candidate must not
    /// materialize a stdlib there: that is a full stdlib copy written into a *shared* cache
    /// directory, per package, and never reclaimed — all for a read-only compatibility check that
    /// may well reject the candidate. Candidates are evaluated in the staged board's workspace
    /// instead, which is where a selected package actually builds.
    #[test]
    fn evaluating_a_candidate_writes_nothing_into_the_candidate_package() {
        let fixture = ReuseFixture::compatible();
        // Make the package its own workspace root, which is the condition that causes the stdlib
        // copy. A real cached package reaches it by having no workspace-declaring ancestor at all;
        // under the fixture's board repo the nearest explicit `[workspace]` wins, so the marker goes
        // on the package to put resolution in the same place.
        fs::write(fixture.package.join("pcb.toml"), "[workspace]\n").unwrap();
        let component = source_component(None);

        let plan = fixture.run_default(&component, &["ABC-1.zen"]).unwrap();

        assert!(
            plan.is_some(),
            "the candidate must still be selected when evaluated outside its own workspace"
        );
        assert!(
            !fixture.package.join(".pcb").exists(),
            "evaluation wrote {} into the candidate package",
            fixture.package.join(".pcb").display()
        );
    }

    #[test]
    fn rejects_candidate_whose_part_manufacturer_differs_from_source() {
        let fixture = ReuseFixture::compatible();
        // Same MPN, different manufacturer: reuse must not substitute.
        let component = source_component(Some("Other Corp"));

        assert!(
            fixture
                .run_default(&component, &["ABC-1.zen"])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn falls_back_to_generated_component_when_multiple_entrypoints_share_one_exact_mpn() {
        // Two byte-identical but distinct entrypoints: both are electrically compatible, so the
        // gate under test is the ambiguity arm, not any per-candidate rejection.
        let fixture = ReuseFixture::new(&[
            ("ABC-1.zen", COMPATIBLE_ZEN),
            ("ABC-1-alt.zen", COMPATIBLE_ZEN),
        ]);
        let component = source_component(None);

        // Ambiguity must never silently pick one of the two: no plan, and nothing staged. Both
        // assertions fail if the ambiguity arm is removed and a candidate is selected instead.
        let outcome = fixture
            .run_outcome(
                &component,
                vec![candidate(&["ABC-1.zen", "ABC-1-alt.zen"], "Acme")],
            )
            .unwrap();
        assert!(!fixture.board.join("components").join("registry").exists());
        // The number of competitors comes back so the report can record it per refdes, which is what
        // distinguishes this fallback from "no registry module matched".
        assert!(
            matches!(outcome, RegistryReuseOutcome::Ambiguous(2)),
            "ambiguity must report its competitor count and stage nothing"
        );

        // Each entrypoint on its own is compatible, so the fallback is the ambiguity, not a defect
        // in the fixture. A single compatible entrypoint records no ambiguity.
        let outcome = ReuseFixture::compatible()
            .run_outcome(&component, vec![candidate(&["ABC-1.zen"], "Acme")])
            .unwrap();
        assert!(matches!(outcome, RegistryReuseOutcome::Reused(_)));
    }

    #[test]
    fn rejects_candidate_whose_evaluated_part_mpn_differs_from_source() {
        // Only the declared `mpn=` differs; the indexed record still claims the source MPN.
        let fixture = ReuseFixture::new(&[(
            "ABC-1.zen",
            &COMPATIBLE_ZEN.replace(r#"mpn="ABC-1""#, r#"mpn="XYZ-9""#),
        )]);
        let component = source_component(Some("Acme"));

        assert!(
            fixture
                .run_default(&component, &["ABC-1.zen"])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_candidate_whose_component_has_no_part_metadata() {
        // No `mpn`/`manufacturer` at all. The source and the indexed record also name no
        // manufacturer, so every manufacturer gate is satisfied and only the missing Part rejects.
        let fixture = ReuseFixture::new(&[(
            "ABC-1.zen",
            r#"P1 = io(Net)
Component(
    name="ABC_1",
    footprint="TEST.kicad_mod",
    pin_defs={"P1": "1"},
    pins={"P1": P1},
)
"#,
        )]);
        let component = source_component(None);

        assert!(
            fixture
                .run(&component, vec![candidate(&["ABC-1.zen"], "")])
                .unwrap()
                .is_none()
        );
        // The entrypoint is otherwise sound: it evaluates, and it really has no Part.
        let evaluated = fixture.evaluate(&fixture.entrypoint("ABC-1.zen")).unwrap();
        assert_eq!(evaluated.part_mpn, None);
    }

    #[test]
    fn rejects_candidate_entrypoint_with_more_than_one_physical_component() {
        let fixture = ReuseFixture::new(&[(
            "ABC-1.zen",
            r#"P1 = io(Net)
Component(
    name="ABC_1",
    footprint="TEST.kicad_mod",
    pin_defs={"P1": "1"},
    pins={"P1": P1},
    mpn="ABC-1",
    manufacturer="Acme",
)
Component(
    name="ABC_1_EXTRA",
    footprint="TEST.kicad_mod",
    pin_defs={"P1": "1"},
    pins={"P1": P1},
    mpn="ABC-1",
    manufacturer="Acme",
)
"#,
        )]);
        let component = source_component(Some("Acme"));

        let error = error_message(fixture.evaluate(&fixture.entrypoint("ABC-1.zen")));
        assert!(
            error.contains("exactly one physical component"),
            "unexpected error: {error}"
        );
        assert!(
            fixture
                .run_default(&component, &["ABC-1.zen"])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    #[cfg(unix)]
    fn rejects_candidate_package_containing_symlink() {
        use std::os::unix::fs::symlink;

        let fixture = ReuseFixture::compatible();
        let outside = fixture.board.parent().unwrap().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        // `vendor/` is skipped by the footprint search but not by the copy-safety walk, so this
        // symlink is only visible to the tree validation.
        fs::create_dir(fixture.package.join("vendor")).unwrap();
        symlink(&outside, fixture.package.join("vendor").join("dep.txt")).unwrap();
        let component = source_component(Some("Acme"));

        let error = error_message(validate_registry_package_tree(&fixture.package));
        assert!(
            error.contains("unsupported symlink"),
            "unexpected error: {error}"
        );
        assert!(
            fixture
                .run_default(&component, &["ABC-1.zen"])
                .unwrap()
                .is_none()
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
    }

    /// One module reached through several candidate records is one choice, not competing ones. Two
    /// symbols of a module that share an MPN, or the same module in two cached indexes, each carry the
    /// module's whole entrypoint list, so the identical entrypoint arrives more than once.
    #[test]
    fn one_entrypoint_reached_twice_is_not_ambiguous() {
        let fixture = ReuseFixture::compatible();
        let component = source_component(Some("Acme"));

        // Two records, same module, same single entrypoint.
        let outcome = fixture
            .run_outcome(
                &component,
                vec![
                    candidate(&["ABC-1.zen"], "Acme"),
                    candidate(&["ABC-1.zen"], "Acme"),
                ],
            )
            .unwrap();

        assert!(
            matches!(outcome, RegistryReuseOutcome::Reused(_)),
            "one distinct entrypoint must count once, not as two competing matches"
        );
    }

    /// A vendored registry package left by an earlier import must be refreshable. Skipping the copy
    /// whenever the destination directory existed meant `--force` could never replace a stale or
    /// damaged package, so a bad copy would be reused indefinitely.
    #[test]
    fn force_refreshes_an_already_vendored_registry_package() {
        let fixture = ReuseFixture::compatible();
        let component = source_component(Some("Acme"));

        let plan = fixture
            .run_default(&component, &["ABC-1.zen"])
            .unwrap()
            .expect("first run vendors the package");
        let vendored = fixture.board.join(&plan.staged_entrypoint);
        assert!(vendored.is_file());

        // Stand in for a stale or damaged copy from an earlier import.
        fs::write(&vendored, "STALE").unwrap();

        // Without `--force` the existing content still wins, as it does for every other output.
        fixture
            .run_default(&component, &["ABC-1.zen"])
            .unwrap()
            .expect("plan is still produced");
        assert_eq!(
            fs::read_to_string(&vendored).unwrap(),
            "STALE",
            "the default mode must not overwrite what is already there"
        );

        let outcome = fixture
            .run_outcome_with_force(&component, vec![candidate(&["ABC-1.zen"], "Acme")], true)
            .unwrap();

        assert!(matches!(outcome, RegistryReuseOutcome::Reused(_)));
        assert_ne!(
            fs::read_to_string(&vendored).unwrap(),
            "STALE",
            "--force must refresh the vendored package from the cache"
        );
        assert!(
            fs::read_to_string(&vendored)
                .unwrap()
                .contains("Component("),
            "the refreshed file must be the real package entrypoint again"
        );
    }

    /// The point of comparing geometry: a differently *named* footprint that is the same copper is a
    /// valid substitute. Library naming is not part identity — one library calls the u-blox SAM-M10Q
    /// land pattern `ublox_SAM-M8Q` and another calls it `SAM-M10Q-00B`.
    #[test]
    fn accepts_a_differently_named_footprint_with_the_same_land_pattern() {
        let fixture = ReuseFixture::compatible();
        // Same pad, same place, same size — only the footprint name differs from the candidate's.
        let component = with_footprint_geometry(
            source_component(Some("Acme")),
            "OtherLib:COMPLETELY_DIFFERENT_NAME",
            &one_pad_footprint("COMPLETELY_DIFFERENT_NAME"),
        );

        let plan = fixture
            .run_default(&component, &["ABC-1.zen"])
            .unwrap()
            .expect("a same-land-pattern candidate must be substitutable despite the name");

        assert!(plan.module_path.starts_with("components/registry/"));
    }

    /// The converse, and the reason the gate cannot simply be dropped: a matching *name* over
    /// different copper must still be rejected.
    #[test]
    fn rejects_an_identically_named_footprint_with_a_different_land_pattern() {
        let fixture = ReuseFixture::compatible();
        let moved = one_pad_footprint("TEST").replace("(at 0 0)", "(at 1.5 0)");
        let component =
            with_footprint_geometry(source_component(Some("Acme")), "Local:TEST", &moved);

        assert!(
            fixture
                .run_default(&component, &["ABC-1.zen"])
                .unwrap()
                .is_none(),
            "a pad in a different place is a different land pattern, whatever it is called"
        );
    }

    /// With no resolved source geometry there is nothing to compare, so the older name-and-pad-number
    /// evidence still governs. This is the path a bundled-stdlib footprint takes.
    #[test]
    fn without_source_geometry_the_footprint_name_still_gates() {
        let fixture = ReuseFixture::compatible();
        let mut component = source_component(Some("Acme"));
        component.netlist.footprint = Some("Local:NOT_TEST".to_string());
        assert!(component.layout.is_none(), "no resolved geometry");

        assert!(
            fixture
                .run_default(&component, &["ABC-1.zen"])
                .unwrap()
                .is_none(),
            "without geometry the name is the only evidence, so it must still be required"
        );
    }
}
