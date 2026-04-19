// Module implementing KiCad net-list export functionality for `pcb_sch::Schematic`.

use pathdiff::diff_paths;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::{AttributeValue, InstanceKind, InstanceRef, PACKAGE_URI_PREFIX, Schematic};

#[derive(Debug)]
struct CompInfo<'a> {
    reference: InstanceRef,
    instance: &'a crate::Instance,
    hier_name: String, // dot-separated instance path
}

#[derive(Debug, Clone)]
struct Node {
    refdes: String,
    pad: String,
}

#[derive(Debug)]
struct NetInfo {
    code: u32,
    name: String,
    nodes: Vec<Node>,
}

#[derive(Default, Debug)]
struct LibPartInfo {
    pins: Vec<(String, String)>, // (num, name)
}

/// Escape quotes in a string for KiCad S-expression format.
/// In S-expressions, quotes within strings are escaped with a backslash.
fn escape_kicad_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Format an array of AttributeValues as a comma-separated string.
pub(crate) fn format_array_as_csv(arr: &[AttributeValue]) -> String {
    arr.iter()
        .map(|v| match v {
            AttributeValue::String(s) => s.clone(),
            AttributeValue::Number(n) => n.to_string(),
            AttributeValue::Boolean(b) => b.to_string(),
            AttributeValue::Port(s) => s.clone(),
            AttributeValue::Array(_) => "[]".to_string(), // Nested arrays not supported
            AttributeValue::Json(j) => serde_json::to_string(j).unwrap_or("{}".to_owned()),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Export the provided [`Schematic`] into a KiCad-compatible net-list (S-expression, E-series).
///
/// The implementation focuses on the mandatory `(components …)` and `(nets …)` sections that
/// KiCad PCB-new needs to import a net-list.  All footprints are set to a dummy `lib:UNKNOWN`
/// if the component instance doesn't specify one.
///
/// This expects component instances to already have `reference_designator` assigned (typically
/// done during schematic conversion). If you construct a [`Schematic`] manually, call
/// [`Schematic::assign_reference_designators`](crate::Schematic::assign_reference_designators)
/// before exporting.
pub fn to_kicad_netlist(sch: &Schematic) -> String {
    let mut components: Vec<CompInfo<'_>> = Vec::new();
    for (inst_ref, inst) in &sch.instances {
        if inst.kind == InstanceKind::Component {
            let hier = inst_ref.instance_path.join(".");
            components.push(CompInfo {
                reference: inst_ref.clone(),
                instance: inst,
                hier_name: hier,
            });
        }
    }
    // Ensure deterministic output ordering.
    // Use natural ordering so `R2` sorts before `R10`.
    components.sort_by(|a, b| natord::compare(&a.hier_name, &b.hier_name));

    //---------------------------------------------------------------------
    // 2. Collect nets.
    //---------------------------------------------------------------------

    let mut nets: HashMap<String, NetInfo> = HashMap::new();

    for (net_name, net) in &sch.nets {
        let mut info = NetInfo {
            code: 0,
            name: net_name.clone(),
            nodes: Vec::new(),
        };

        for port_ref in &net.ports {
            // Determine the component instance that owns this port by longest-prefix match.
            let Some(comp_ref) = sch.component_ref_for_port(port_ref) else {
                continue; // malformed – skip
            };
            let refdes = sch
                .instances
                .get(&comp_ref)
                .and_then(|inst| inst.reference_designator.as_deref())
                .unwrap_or_else(|| {
                    panic!(
                        "component {} is missing a reference designator; call Schematic::assign_reference_designators() first",
                        comp_ref
                    )
                });

            // Fetch pad number from port instance attributes.
            let pads: Vec<String> = sch
                .instances
                .get(port_ref)
                .and_then(|inst| inst.attributes.get("pads"))
                .and_then(|av| match av {
                    AttributeValue::Array(arr) => Some(arr),
                    _ => None,
                })
                .map(|arr| {
                    arr.iter()
                        .filter_map(|av| match av {
                            AttributeValue::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();

            for pad in pads {
                info.nodes.push(Node {
                    refdes: refdes.to_owned(),
                    pad,
                });
            }
        }

        nets.insert(info.name.clone(), info);
    }

    //---------------------------------------------------------------------
    // 3. Emit S-expression.
    //---------------------------------------------------------------------
    let mut out = String::new();

    writeln!(out, "(export (version \"E\")").unwrap();
    writeln!(out, "  (design").unwrap();
    writeln!(out, "    (source \"unknown\")").unwrap();
    writeln!(out, "    (date \"\")").unwrap();
    writeln!(out, "    (tool \"pcb\"))").unwrap();

    //---------------- components ----------------
    writeln!(out, "  (components").unwrap();
    for comp in &components {
        let refdes = comp.instance.reference_designator.as_deref().unwrap_or_else(|| {
            panic!(
                "component {} is missing a reference designator; call Schematic::assign_reference_designators() first",
                comp.reference
            )
        });
        let value_field = comp
            .instance
            .attributes
            .get("mpn")
            .or_else(|| comp.instance.attributes.get("Value"))
            .or_else(|| comp.instance.attributes.get("Val"))
            .or_else(|| comp.instance.attributes.get("type"))
            .and_then(|av| match av {
                AttributeValue::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("?");
        let fp_attr = comp
            .instance
            .attributes
            .get("footprint")
            .and_then(|av| match av {
                AttributeValue::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("UNKNOWN:UNKNOWN");
        let (fp_string, _lib_info) =
            format_footprint_with_package_roots(fp_attr, &sch.package_roots);

        writeln!(out, "    (comp (ref \"{}\")", escape_kicad_string(refdes)).unwrap();
        writeln!(
            out,
            "      (value \"{}\")",
            escape_kicad_string(value_field)
        )
        .unwrap();
        writeln!(
            out,
            "      (footprint \"{}\")",
            escape_kicad_string(&fp_string)
        )
        .unwrap();
        writeln!(
            out,
            "      (libsource (lib \"lib\") (part \"{}\") (description \"unknown\"))",
            escape_kicad_string(value_field)
        )
        .unwrap();
        // Deterministic UUID from hierarchical name.
        let ts_uuid = Uuid::new_v5(&Uuid::NAMESPACE_URL, comp.hier_name.as_bytes());
        writeln!(
            out,
            "      (sheetpath (names \"{}\") (tstamps \"{}\"))",
            escape_kicad_string(&comp.hier_name),
            ts_uuid
        )
        .unwrap();
        writeln!(out, "      (tstamps \"{ts_uuid}\")").unwrap();

        // Explicitly add the standard KiCad "Reference" property pointing to the component's
        // reference designator.  This ensures the field is always present and consistent
        // irrespective of user-specified attributes.
        writeln!(
            out,
            "      (property (name \"Reference\") (value \"{}\"))",
            escape_kicad_string(refdes)
        )
        .unwrap();

        // Additional attributes – sort keys for deterministic output
        let mut attr_pairs: Vec<_> = comp.instance.attributes.iter().collect();
        attr_pairs.sort_by(|a, b| a.0.cmp(b.0));

        for (key, val) in attr_pairs {
            let val_str = match val {
                AttributeValue::String(s) => s.clone(),
                AttributeValue::Number(n) => n.to_string(),
                AttributeValue::Boolean(b) => b.to_string(),
                AttributeValue::Port(s) => s.clone(),
                AttributeValue::Array(arr) => format_array_as_csv(arr),
                AttributeValue::Json(j) => serde_json::to_string(j).unwrap_or("{}".to_owned()),
            };
            // Skip keys already encoded separately, internal keys, symbol metadata, or keys starting with __
            if [
                "mpn",
                "type",
                "footprint",
                "prefix",
                "Reference",
                "symbol_name",
                "symbol_path",
            ]
            .contains(&key.as_str())
                || key.starts_with("__")
            {
                continue;
            }
            writeln!(
                out,
                "      (property (name \"{}\") (value \"{}\"))",
                escape_kicad_string(key),
                escape_kicad_string(&val_str)
            )
            .unwrap();
        }
        writeln!(out, "    )").unwrap();
    }
    writeln!(out, "  )").unwrap();

    //---------------------------------------------------------------------
    // 4. Libparts (unique component type definitions) – simplified version.
    //---------------------------------------------------------------------
    let mut libparts: HashMap<String, LibPartInfo> = HashMap::new();

    for comp in &components {
        let mpn = comp
            .instance
            .attributes
            .get("mpn")
            .and_then(|v| match v {
                AttributeValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or("?".to_owned());
        let entry = libparts.entry(mpn.clone()).or_default();

        // Collect pins from children
        if let Some(ComponentChildren { pins }) = collect_pins_for_component(sch, &comp.reference) {
            for (pad, name) in pins {
                entry.pins.push((pad, name));
            }
        }
    }

    // Deduplicate and sort pins within each libpart.
    for info in libparts.values_mut() {
        let mut uniq: HashSet<(String, String)> = HashSet::new();
        info.pins.retain(|p| uniq.insert(p.clone()));
        info.pins.sort_by(|a, b| a.0.cmp(&b.0));
    }

    writeln!(out, "  (libparts").unwrap();
    let mut libparts_vec: Vec<_> = libparts.into_iter().collect();
    libparts_vec.sort_by(|a, b| a.0.cmp(&b.0));
    for (mpn, info) in libparts_vec {
        writeln!(
            out,
            "    (libpart (lib \"lib\") (part \"{}\")",
            escape_kicad_string(&mpn)
        )
        .unwrap();
        writeln!(out, "      (description \"\")").unwrap();
        writeln!(out, "      (docs \"~\")").unwrap();
        writeln!(out, "      (footprints").unwrap();
        writeln!(out, "        (fp \"*\"))").unwrap();
        writeln!(out, "      (pins").unwrap();
        for (num, name) in info.pins {
            writeln!(
                out,
                "        (pin (num \"{}\") (name \"{}\") (type \"stereo\"))",
                escape_kicad_string(&num),
                escape_kicad_string(&name)
            )
            .unwrap();
        }
        writeln!(out, "      )").unwrap();
        writeln!(out, "    )").unwrap();
    }
    writeln!(out, "  )").unwrap();

    //---------------------------------------------------------------------
    // 5. Nets section.
    //---------------------------------------------------------------------
    writeln!(out, "  (nets").unwrap();
    let mut net_vec: Vec<_> = nets.into_iter().collect();
    net_vec.sort_by(|a, b| a.0.cmp(&b.0));
    for (code, (_name, info)) in (1_u32..).zip(net_vec.iter_mut()) {
        info.code = code;
    }
    for (_name, info) in net_vec {
        // Sort nodes for deterministic ordering.
        let mut sorted_nodes = info.nodes.clone();
        sorted_nodes.sort_by(|a, b| {
            let ord = natord::compare(&a.refdes, &b.refdes);
            if ord == std::cmp::Ordering::Equal {
                a.pad.cmp(&b.pad)
            } else {
                ord
            }
        });

        writeln!(
            out,
            "    (net (code \"{}\") (name \"{}\")",
            info.code,
            escape_kicad_string(&info.name)
        )
        .unwrap();
        for node in sorted_nodes {
            writeln!(
                out,
                "      (node (ref \"{}\") (pin \"{}\") (pintype \"stereo\"))",
                escape_kicad_string(&node.refdes),
                escape_kicad_string(&node.pad)
            )
            .unwrap();
        }
        writeln!(out, "    )").unwrap();
    }
    writeln!(out, "  )").unwrap();
    writeln!(out, ")").unwrap();

    out
}

// Helper returning all pins (pad, name) for a given component reference.
struct ComponentChildren {
    pins: Vec<(String, String)>,
}

fn collect_pins_for_component(
    sch: &Schematic,
    comp_ref: &InstanceRef,
) -> Option<ComponentChildren> {
    let comp_inst = sch.instances.get(comp_ref)?;
    let mut pins = Vec::new();
    for child_ref in comp_inst.children.values() {
        let child_inst = sch.instances.get(child_ref)?;
        if child_inst.kind == InstanceKind::Port
            && let Some(AttributeValue::Array(pads)) = child_inst.attributes.get("pads")
        {
            for pad in pads {
                if let AttributeValue::String(pad) = pad {
                    let pin_name = sch
                        .component_ref_and_pin_for_port(child_ref)
                        .and_then(|(owner_ref, pin_name)| {
                            (owner_ref == *comp_ref).then_some(pin_name)
                        })
                        .unwrap_or_else(|| pad.clone());
                    pins.push((pad.clone(), pin_name));
                }
            }
        }
    }
    Some(ComponentChildren { pins })
}

// -------------------------------------------------------------------------------------------------
// Footprint conversion helper
// -------------------------------------------------------------------------------------------------

/// Convert footprint strings that may point to a `.kicad_mod` file into a KiCad `lib:fp` identifier.
/// Returns the (possibly modified) footprint string and optional `(lib_name, dir)` tuple that can be
/// used to populate the fp-lib-table.
pub fn format_footprint(fp: &str) -> (String, Option<(String, PathBuf)>) {
    format_footprint_with_package_roots(fp, &BTreeMap::new())
}

/// Package-aware variant of [`format_footprint`].
pub fn format_footprint_with_package_roots(
    fp: &str,
    package_roots: &BTreeMap<String, PathBuf>,
) -> (String, Option<(String, PathBuf)>) {
    if is_kicad_lib_fp(fp) {
        return (fp.to_owned(), None);
    }

    let resolved = if fp.starts_with(PACKAGE_URI_PREFIX) {
        crate::resolve_package_uri(fp, package_roots).unwrap_or_else(|_| PathBuf::from(fp))
    } else {
        PathBuf::from(fp)
    };

    format_resolved_footprint_path(resolved.as_path(), package_roots)
}

/// Fallible package-aware formatter used by layout prep, which should reject
/// unresolved package URIs before invoking KiCad.
pub fn try_format_footprint_with_package_roots(
    fp: &str,
    package_roots: &BTreeMap<String, PathBuf>,
) -> anyhow::Result<(String, Option<(String, PathBuf)>)> {
    if is_kicad_lib_fp(fp) {
        return Ok((fp.to_owned(), None));
    }

    let resolved = if fp.starts_with(PACKAGE_URI_PREFIX) {
        crate::resolve_package_uri(fp, package_roots)?
    } else {
        PathBuf::from(fp)
    };

    Ok(format_resolved_footprint_path(
        resolved.as_path(),
        package_roots,
    ))
}

fn format_resolved_footprint_path(
    p: &Path,
    package_roots: &BTreeMap<String, PathBuf>,
) -> (String, Option<(String, PathBuf)>) {
    let Some(stem_os) = p.file_stem() else {
        return ("UNKNOWN:UNKNOWN".to_owned(), None);
    };
    let footprint_name = stem_os.to_string_lossy().to_string();
    let dir = p
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let lib_name =
        footprint_library_name(p, package_roots).unwrap_or_else(|| footprint_name.clone());
    (
        format!("{lib_name}:{footprint_name}"),
        Some((lib_name, dir)),
    )
}

fn footprint_library_name(p: &Path, package_roots: &BTreeMap<String, PathBuf>) -> Option<String> {
    let parent = p.parent()?;
    if let Some((package_coord, package_root)) = package_coord_for_path(p, package_roots)
        && let Ok(rel_dir) = parent.strip_prefix(&package_root)
        && let Some(package_name) = package_library_name(&package_coord, rel_dir)
    {
        return Some(package_name);
    }

    fallback_library_name(parent)
}

/// Derive a deterministic KiCad library nickname from package identity and
/// the library directory relative to that package root.
///
/// Rules:
/// - Strip only the hostname from the package coordinate.
/// - Strip only the final `.pretty` suffix from the relative library path.
/// - Sanitize path segments with underscores.
/// - If the package coordinate ends with `@version`, move that version suffix
///   to the end of the full rendered nickname.
fn package_library_name(package_coord: &str, rel_dir: &Path) -> Option<String> {
    let (package_path, version) = split_package_version(strip_package_host(package_coord));
    let mut parts = package_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(sanitize_nickname_segment)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();

    parts.extend(
        relative_library_segments(rel_dir)
            .into_iter()
            .map(|segment| sanitize_nickname_segment(&segment))
            .filter(|segment| !segment.is_empty()),
    );

    let mut nickname = parts.join("_");

    if nickname.is_empty() {
        return None;
    }

    if let Some(version) = version {
        let version = sanitize_nickname_segment(version);

        if !version.is_empty() {
            nickname.push('@');
            nickname.push_str(&version);
        }
    }

    Some(nickname)
}

fn split_package_version(package_coord: &str) -> (&str, Option<&str>) {
    let Some((head, tail)) = package_coord.rsplit_once('/') else {
        return split_final_version_suffix(package_coord);
    };

    let (last, version) = split_final_version_suffix(tail);

    let Some(version) = version else {
        return (package_coord, None);
    };

    if head.is_empty() {
        (last, Some(version))
    } else {
        let package_path = &package_coord[..head.len() + 1 + last.len()];
        (package_path, Some(version))
    }
}

fn relative_library_segments(rel_dir: &Path) -> Vec<String> {
    let mut segments: Vec<String> = rel_dir
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();

    if let Some(last) = segments.last_mut() {
        *last = strip_final_pretty_suffix(last).to_owned();
    }

    segments
}

fn sanitize_nickname_segment(segment: &str) -> String {
    segment
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' => ch,
            _ => '_',
        })
        .collect()
}

fn strip_package_host(package_coord: &str) -> &str {
    if let Some((host, rest)) = package_coord.split_once('/')
        && host.contains('.')
        && !rest.is_empty()
    {
        return rest;
    }

    package_coord
}

fn fallback_library_name(parent: &Path) -> Option<String> {
    if let Some(pretty_name) = parent
        .file_stem()
        .filter(|_| parent.extension().is_some_and(|ext| ext == "pretty"))
        .map(|stem| stem.to_string_lossy().to_string())
        .map(|name| strip_final_pretty_suffix(&name).to_owned())
        .filter(|name| !name.is_empty())
    {
        return Some(pretty_name);
    }

    parent
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .filter(|name| !name.is_empty())
}

fn package_coord_for_path(
    path: &Path,
    package_roots: &BTreeMap<String, PathBuf>,
) -> Option<(String, PathBuf)> {
    package_roots
        .iter()
        .filter_map(|(coord, root)| {
            if !path.starts_with(root) {
                return None;
            }
            Some((root.components().count(), coord.clone(), root.clone()))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, package_coord, root)| (package_coord, root))
}

fn split_final_version_suffix(segment: &str) -> (&str, Option<&str>) {
    match segment.rsplit_once('@') {
        Some((name, version)) if !name.is_empty() && !version.is_empty() => (name, Some(version)),
        _ => (segment, None),
    }
}

fn strip_final_pretty_suffix(name: &str) -> &str {
    name.strip_suffix(".pretty").unwrap_or(name)
}

/// Determine whether a given string is a KiCad `lib:footprint` reference rather than a file path.
///
/// The heuristic is:
/// 1. The string contains exactly one `:` splitting it into `(lib, footprint)`.
/// 2. Neither half contains path separators (`/` or `\`).
/// 3. The left half is not a single-letter Windows drive designator such as `C`.
fn is_kicad_lib_fp(s: &str) -> bool {
    if let Some((lib, fp)) = s.split_once(':') {
        // Filter out Windows drive prefixes like "C:".
        if lib.len() == 1 && lib.chars().all(|c| c.is_ascii_alphabetic()) {
            return false;
        }

        // Any path separator indicates this is still a filesystem path.
        if lib.contains('/') || lib.contains('\\') || fp.contains('/') || fp.contains('\\') {
            return false;
        }

        true
    } else {
        false
    }
}

// -------------------------------------------------------------------------------------------------
// Footprint library table (fp-lib-table) serialization helper
// -------------------------------------------------------------------------------------------------

/// Serialise the provided footprint library map into the KiCad `(fp_lib_table ...)` format.
///
/// The `libs` argument maps *library names* to their **absolute** directory path on disk.
/// The emitted URIs are made project-relative by prefixing them with `${KIPRJMOD}` so that
/// the generated table remains portable when the project directory is moved.
pub fn serialize_fp_lib_table(layout_dir: &Path, libs: &HashMap<String, PathBuf>) -> String {
    let mut table = String::new();
    table.push_str("(fp_lib_table\n");
    table.push_str("  (version 7)\n");

    // Determine an absolute base directory for diffing – if `layout_dir` is
    // relative (e.g. just "layout"), anchor it to the current working
    // directory so `diff_paths` has a common root with absolute `dir_path`s.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let base_dir = if layout_dir.is_absolute() {
        layout_dir.to_path_buf()
    } else {
        cwd.join(layout_dir)
    };

    // Collect libraries into a vector and sort by the library name to guarantee
    // deterministic output ordering.  HashMap iteration order is non-deterministic
    // (and even differs between architectures / Rust versions), so iterating over it
    // directly would yield non-reproducible `fp-lib-table` files.

    let mut libs_sorted: Vec<(&String, &PathBuf)> = libs.iter().collect();
    libs_sorted.sort_by(|a, b| a.0.cmp(b.0));

    for (lib_name, dir_path) in libs_sorted {
        // Compute relative path for portability.
        // 1st attempt: relative to `base_dir` (layout directory).
        // 2nd attempt: relative to project root (`cwd`).
        // Fallback: absolute path.
        let rel_path = diff_paths(dir_path, &base_dir)
            .or_else(|| diff_paths(dir_path, &cwd))
            .unwrap_or_else(|| dir_path.clone());

        // Ensure we don't produce Windows-specific prefixes or double slashes in the URI.
        let mut path_str = rel_path.display().to_string();

        // Convert any back-slashes (Windows) to forward slashes for KiCad.
        path_str = path_str.replace('\\', "/");

        // Strip Windows extended-length prefix (e.g. "//?/C:/...").
        if path_str.starts_with("//?/") {
            path_str = path_str.trim_start_matches("//?/").to_string();
        }

        // Remove all leading slashes to avoid "${KIPRJMOD}//…".
        while path_str.starts_with('/') {
            path_str.remove(0);
        }

        // Construct final URI: use project-relative `${KIPRJMOD}` only for relative paths.
        let uri = if rel_path.is_relative() {
            format!("${{KIPRJMOD}}/{path_str}")
        } else {
            // Absolute path – use it directly.
            path_str.clone()
        };

        table.push_str(&format!(
            "  (lib (name \"{}\") (type \"KiCad\") (uri \"{}\") (options \"\") (descr \"\"))\n",
            escape_kicad_string(lib_name),
            escape_kicad_string(&uri)
        ));
    }

    table.push_str(")\n");
    table
}

/// Convenience wrapper writing an `fp-lib-table` file inside `layout_dir`.
/// If `libs` is empty, no file is written.
pub fn write_fp_lib_table(
    layout_dir: &Path,
    libs: &HashMap<String, PathBuf>,
) -> std::io::Result<()> {
    if libs.is_empty() {
        return Ok(());
    }

    std::fs::create_dir_all(layout_dir)?;
    let table_str = serialize_fp_lib_table(layout_dir, libs);
    let table_path = layout_dir.join("fp-lib-table");
    std::fs::write(&table_path, table_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, HashMap};
    use std::path::Path;

    fn assert_formatted_footprint(
        formatted: (String, Option<(String, PathBuf)>),
        expected_fp: &str,
        expected_lib: &str,
        expected_dir: &str,
    ) {
        assert_eq!(formatted.0, expected_fp);
        assert_eq!(
            formatted.1,
            Some((expected_lib.to_string(), PathBuf::from(expected_dir)))
        );
    }

    #[test]
    fn test_escape_kicad_string() {
        // Test basic string without special characters
        assert_eq!(escape_kicad_string("hello"), "hello");

        // Test string with quotes
        assert_eq!(
            escape_kicad_string("hello \"world\""),
            "hello \\\"world\\\""
        );

        // Test string with backslashes
        assert_eq!(escape_kicad_string("path\\to\\file"), "path\\\\to\\\\file");

        // Test string with both quotes and backslashes
        assert_eq!(
            escape_kicad_string("\"C:\\Program Files\\test\""),
            "\\\"C:\\\\Program Files\\\\test\\\""
        );

        // Test empty string
        assert_eq!(escape_kicad_string(""), "");

        // Test string with multiple quotes
        assert_eq!(escape_kicad_string("\"\"\""), "\\\"\\\"\\\"");
    }

    #[test]
    fn test_is_kicad_lib_fp() {
        // Valid KiCad lib:fp format
        assert!(is_kicad_lib_fp("Resistor_SMD:R_0603_1608Metric"));
        assert!(is_kicad_lib_fp("Capacitor_SMD:C_0805_2012Metric"));

        // Windows paths (should return false)
        assert!(!is_kicad_lib_fp("C:\\path\\to\\footprint.kicad_mod"));
        assert!(!is_kicad_lib_fp("C:footprint.kicad_mod"));

        // Unix paths (should return false)
        assert!(!is_kicad_lib_fp("/path/to/footprint.kicad_mod"));
        assert!(!is_kicad_lib_fp("./relative/path.kicad_mod"));

        // No colon (should return false)
        assert!(!is_kicad_lib_fp("footprint_name"));

        // Multiple colons (should return false since split_once will only match first)
        assert!(is_kicad_lib_fp("lib:footprint:extra")); // This will be treated as lib "lib" and footprint "footprint:extra"
    }

    #[test]
    fn test_format_footprint_for_pretty_library_path() {
        assert_formatted_footprint(
            format_footprint(
                "/tmp/cache/kicad-footprints/Capacitor_SMD.pretty/C_0402_1005Metric.kicad_mod",
            ),
            "Capacitor_SMD:C_0402_1005Metric",
            "Capacitor_SMD",
            "/tmp/cache/kicad-footprints/Capacitor_SMD.pretty",
        );
    }

    #[test]
    fn test_format_footprint_for_kicad_pretty_package_root_uri() {
        let mut package_roots = BTreeMap::new();
        package_roots.insert(
            "gitlab.com/example/libs/footprints@10.0.0".to_string(),
            PathBuf::from("/tmp/vendor/gitlab.com/example/libs/footprints/10.0.0"),
        );

        assert_formatted_footprint(
            format_footprint_with_package_roots(
                "package://gitlab.com/example/libs/footprints@10.0.0/Capacitor_SMD.pretty/C_0402_1005Metric.kicad_mod",
                &package_roots,
            ),
            "example_libs_footprints_Capacitor_SMD@10.0.0:C_0402_1005Metric",
            "example_libs_footprints_Capacitor_SMD@10.0.0",
            "/tmp/vendor/gitlab.com/example/libs/footprints/10.0.0/Capacitor_SMD.pretty",
        );
    }

    #[test]
    fn test_format_footprint_for_plain_directory_path() {
        assert_formatted_footprint(
            format_footprint("/tmp/components/TLV9001IDBVR/SOT95P280X145-5N.kicad_mod"),
            "TLV9001IDBVR:SOT95P280X145-5N",
            "TLV9001IDBVR",
            "/tmp/components/TLV9001IDBVR",
        );
    }

    #[test]
    fn test_format_footprint_preserves_dotted_directory_name() {
        assert_formatted_footprint(
            format_footprint("/tmp/components/lib.v2/SOT95P280X145-5N.kicad_mod"),
            "lib.v2:SOT95P280X145-5N",
            "lib.v2",
            "/tmp/components/lib.v2",
        );
    }

    #[test]
    fn test_format_footprint_for_versioned_package_root_uri() {
        let mut package_roots = BTreeMap::new();
        package_roots.insert(
            "github.com/example/registry/components/ExamplePart@0.3.2".to_string(),
            PathBuf::from("/tmp/vendor/github.com/example/registry/components/ExamplePart/0.3.2"),
        );

        assert_formatted_footprint(
            format_footprint_with_package_roots(
                "package://github.com/example/registry/components/ExamplePart@0.3.2/SOT96P240X115-3N.kicad_mod",
                &package_roots,
            ),
            "example_registry_components_ExamplePart@0.3.2:SOT96P240X115-3N",
            "example_registry_components_ExamplePart@0.3.2",
            "/tmp/vendor/github.com/example/registry/components/ExamplePart/0.3.2",
        );
    }

    #[test]
    fn test_format_footprint_for_workspace_package_root_path() {
        let mut package_roots = BTreeMap::new();
        package_roots.insert(
            "workspace/components/MyPart".to_string(),
            PathBuf::from("/tmp/workspace/components/MyPart"),
        );

        assert_formatted_footprint(
            format_footprint_with_package_roots(
                "/tmp/workspace/components/MyPart/Thing.kicad_mod",
                &package_roots,
            ),
            "workspace_components_MyPart:Thing",
            "workspace_components_MyPart",
            "/tmp/workspace/components/MyPart",
        );
    }

    #[test]
    fn test_package_library_name_cases() {
        assert_eq!(
            package_library_name(
                "github.com/example/workspace/boards/DemoBoard",
                Path::new("components/1u.pretty"),
            ),
            Some("example_workspace_boards_DemoBoard_components_1u".to_string())
        );
        assert_eq!(
            package_library_name("boards/DemoBoard", Path::new("components/1u")),
            Some("boards_DemoBoard_components_1u".to_string())
        );
        assert_eq!(
            package_library_name(
                "github.com/example/workspace/boards/DemoBoard",
                Path::new("components/Murata/1u"),
            ),
            Some("example_workspace_boards_DemoBoard_components_Murata_1u".to_string())
        );
        assert_eq!(
            sanitize_nickname_segment("name_with_underscore"),
            "name_with_underscore"
        );
        assert_eq!(
            sanitize_nickname_segment("name with space"),
            "name_with_space"
        );
        assert_eq!(
            sanitize_nickname_segment("name=with=equals"),
            "name_with_equals"
        );
        assert_eq!(
            package_library_name(
                "github.com/example/libs/footprints@9.0.3",
                Path::new("Resistor_SMD.pretty"),
            ),
            Some("example_libs_footprints_Resistor_SMD@9.0.3".to_string())
        );
    }

    #[test]
    fn test_format_array_as_csv() {
        // Test array of strings
        let arr = vec![
            AttributeValue::String("IDLE".to_string()),
            AttributeValue::String("RUNNING".to_string()),
            AttributeValue::String("STOPPED".to_string()),
        ];
        assert_eq!(format_array_as_csv(&arr), "IDLE, RUNNING, STOPPED");

        // Test array of numbers
        let arr = vec![
            AttributeValue::Number(1.0),
            AttributeValue::Number(2.0),
            AttributeValue::Number(3.0),
        ];
        assert_eq!(format_array_as_csv(&arr), "1, 2, 3");

        // Test mixed array
        let arr = vec![
            AttributeValue::String("a".to_string()),
            AttributeValue::Number(123.0),
            AttributeValue::Boolean(true),
        ];
        assert_eq!(format_array_as_csv(&arr), "a, 123, true");

        // Test empty array
        let arr: Vec<AttributeValue> = vec![];
        assert_eq!(format_array_as_csv(&arr), "");

        // Test single element
        let arr = vec![AttributeValue::String("solo".to_string())];
        assert_eq!(format_array_as_csv(&arr), "solo");
    }

    #[test]
    fn resolves_component_ref_for_split_dotted_port_names() {
        let module_ref = crate::ModuleRef::from_path(Path::new("/tmp/test.zen"), "<root>");

        let comp_ref = InstanceRef::new(
            module_ref.clone(),
            vec!["USB_C".into(), "TVS".into(), "TVS".into()],
        );
        let mut component = crate::Instance::component(module_ref.clone());
        component.reference_designator = Some("D1".to_owned());

        // Simulate a dotted port name after lossy string parsing: "NC.2" -> ["NC", "2"].
        let port_ref = InstanceRef::new(
            module_ref.clone(),
            vec![
                "USB_C".into(),
                "TVS".into(),
                "TVS".into(),
                "NC".into(),
                "2".into(),
            ],
        );
        let mut port = crate::Instance::port(module_ref.clone());
        port.attributes.insert(
            "pads".into(),
            AttributeValue::Array(vec![AttributeValue::String("2".to_owned())]),
        );

        let mut schematic = Schematic::new();
        schematic.add_instance(comp_ref, component);
        schematic.add_instance(port_ref.clone(), port);
        schematic.add_net(crate::Net {
            kind: "Net".to_owned(),
            id: 1,
            name: "USB_C.CC2".to_owned(),
            ports: vec![port_ref],
            properties: HashMap::new(),
        });

        let netlist = to_kicad_netlist(&schematic);
        assert!(netlist.contains("(node (ref \"D1\") (pin \"2\") (pintype \"stereo\"))"));
    }
}
