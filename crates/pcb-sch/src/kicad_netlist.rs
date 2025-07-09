// Module implementing KiCad net-list export functionality for `pcb_sch::Schematic`.

use pathdiff::diff_paths;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::{AttributeValue, InstanceKind, InstanceRef, Schematic};

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

// Helper extracting a prefix string for a component.
fn comp_prefix(inst: &crate::Instance) -> String {
    // Prefer explicit `prefix` attribute if present.
    if let Some(AttributeValue::String(s)) = inst.attributes.get("prefix") {
        return s.clone();
    }
    // Derive from component `type` attribute (e.g. `res` ⇒ `R`).
    if let Some(AttributeValue::String(t)) = inst.attributes.get("type") {
        if let Some(first) = t.chars().next() {
            return first.to_ascii_uppercase().to_string();
        }
    }
    // Fallback `U`.
    "U".to_owned()
}

/// Escape quotes in a string for KiCad S-expression format.
/// In S-expressions, quotes within strings are escaped with a backslash.
fn escape_kicad_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Export the provided [`Schematic`] into a KiCad-compatible net-list (S-expression, E-series).
///
/// The implementation focuses on the mandatory `(components …)` and `(nets …)` sections that
/// KiCad PCB-new needs to import a net-list.  All footprints are set to a dummy `lib:UNKNOWN`
/// if the component instance doesn't specify one.
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
    // Ensure deterministic ordering for subsequent reference designator allocation.
    components.sort_by(|a, b| a.hier_name.cmp(&b.hier_name));

    //---------------------------------------------------------------------
    // 2. Allocate reference designators (REFs)
    //---------------------------------------------------------------------
    let mut ref_counts: HashMap<String, u32> = HashMap::new();
    let mut ref_map: HashMap<&InstanceRef, String> = HashMap::new();

    for comp in &components {
        let prefix = comp_prefix(comp.instance);
        let counter = ref_counts.entry(prefix.clone()).or_default();
        *counter += 1;
        let refdes = format!("{}{}", prefix, *counter);
        ref_map.insert(&comp.reference, refdes);
    }

    //---------------------------------------------------------------------
    // 3. Collect nets.
    //---------------------------------------------------------------------

    let mut nets: HashMap<String, NetInfo> = HashMap::new();

    for (net_name, net) in &sch.nets {
        let mut info = NetInfo {
            code: 0,
            name: net_name.clone(),
            nodes: Vec::new(),
        };

        for port_ref in &net.ports {
            // Determine the component instance that owns this port.
            let mut comp_path = port_ref.instance_path.clone();
            if comp_path.pop().is_none() {
                continue; // malformed – skip
            }
            let comp_ref = InstanceRef {
                module: port_ref.module.clone(),
                instance_path: comp_path,
            };
            let refdes = match ref_map.get(&comp_ref) {
                Some(r) => r.clone(),
                None => continue,
            };

            // Fetch pad number from port instance attributes.
            let pad = sch
                .instances
                .get(port_ref)
                .and_then(|inst| inst.attributes.get("pad"))
                .and_then(|av| match av {
                    AttributeValue::String(s) => Some(s.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "?".to_owned());

            info.nodes.push(Node { refdes, pad });
        }

        nets.insert(info.name.clone(), info);
    }

    //---------------------------------------------------------------------
    // 4. Emit S-expression.
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
        let refdes = &ref_map[&comp.reference];
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
        let (fp_string, _lib_info) = format_footprint(fp_attr);

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
                AttributeValue::Physical(s) => s.clone(),
                AttributeValue::Port(s) => s.clone(),
                AttributeValue::Array(arr) => serde_json::to_string(arr).unwrap_or("[]".to_owned()),
            };
            // Skip keys already encoded separately, internal keys, or keys starting with __
            if ["mpn", "type", "footprint", "prefix", "Reference"].contains(&key.as_str())
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
    // 5. Libparts (unique component type definitions) – simplified version.
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
    // 6. Nets section.
    //---------------------------------------------------------------------
    writeln!(out, "  (nets").unwrap();
    let mut net_vec: Vec<_> = nets.into_iter().collect();
    net_vec.sort_by(|a, b| a.0.cmp(&b.0));
    let mut code: u32 = 1;
    for (_name, info) in &mut net_vec {
        info.code = code;
        code += 1;
    }
    for (_name, info) in net_vec {
        // Sort nodes for deterministic ordering.
        let mut sorted_nodes = info.nodes.clone();
        sorted_nodes.sort_by(|a, b| {
            let ord = a.refdes.cmp(&b.refdes);
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
        if child_inst.kind == InstanceKind::Port {
            if let Some(AttributeValue::String(pad)) = child_inst.attributes.get("pad") {
                let pin_name = child_ref
                    .instance_path
                    .last()
                    .cloned()
                    .unwrap_or_else(|| pad.clone());
                pins.push((pad.clone(), pin_name));
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
    if is_kicad_lib_fp(fp) {
        return (fp.to_owned(), None);
    }
    let p = Path::new(fp);
    let Some(stem_os) = p.file_stem() else {
        return ("UNKNOWN:UNKNOWN".to_owned(), None);
    };
    let stem = stem_os.to_string_lossy();
    let lib_name = stem.to_string();
    let footprint_name = stem.to_string();
    let dir = p
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    (
        format!("{lib_name}:{footprint_name}"),
        Some((lib_name, dir)),
    )
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
        let mut uri = if rel_path.is_relative() {
            format!("${{KIPRJMOD}}/{path_str}")
        } else {
            // Absolute path – use it directly.
            path_str.clone()
        };

        if !uri.ends_with('/') {
            uri.push('/');
        }

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
}
