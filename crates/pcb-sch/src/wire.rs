//! Persisted schematic wire comments (`# pcb:wire` / `# pcb:wire-meta`).
//!
//! Generated schematics persist their wire geometry as trailing comments in the
//! source `.zen` file, in a versioned block that lives *above* the `# pcb:sch`
//! position block:
//!
//! ```text
//! # pcb:wire-meta v1 nethash=0123456789abcdef
//! # pcb:wire v1 GND R_PULLUP.R@2 GND.0 723.9000,88.9000;723.9000,101.6000
//! # pcb:sch R_PULLUP.R x=723.9000 y=88.9000 rot=180
//! ```
//!
//! The position above the `# pcb:sch` block is load-bearing: the position
//! parser ([`crate::position::parse_position_comments`]) scans bottom-up and
//! stops at the first non-`# pcb:sch` line, and position saves truncate only
//! from that block start. Wire lines below the block would make old editors
//! parse zero positions; lines above survive old-editor saves verbatim.
//!
//! # Grammar (v1)
//!
//! * Header: `# pcb:wire-meta v1 nethash=<16 lowercase hex chars>` where the
//!   nethash is the first 16 hex chars of the SHA-256 over the canonical net
//!   partition (sorted `<net_name>\t<component_key>.<pin_number>` lines, one
//!   per component pin, joined with `\n`).
//! * Wire line: `# pcb:wire v1 <net_name> <epA> <epB> <x1,y1;x2,y2;...>`.
//! * Endpoint refs: `<component_key>@<pin_number>` (component pin; the
//!   component key is the instance path relative to the root, in the same form
//!   as `# pcb:sch` component comment keys) or `<NET>.<index>` (net-symbol
//!   endpoint, matching the `# pcb:sch` net-symbol key form).
//! * Coordinates use the `# pcb:sch` position convention: 0.1 mm units,
//!   Y-down, formatted `%.4f`.
//!
//! # Version tolerance
//!
//! A parser that encounters an unparseable `# pcb:wire*` line, or one with an
//! unknown version, MUST preserve the line verbatim, MUST NOT treat its net as
//! persisted, and MUST NOT strip it on save. Such lines are carried in
//! [`WireBlock::preserved_lines`] and re-emitted by [`format_wire_block`].
//! Unknown `# pcb:*` prefixes other than `# pcb:wire*` are never touched by
//! this module.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::Schematic;

/// Version emitted by this writer.
pub const WIRE_VERSION: u32 = 1;

/// Prefix of the wire block header line.
pub const WIRE_META_PREFIX: &str = "# pcb:wire-meta";

/// Prefix shared by every wire-block line (including the header).
pub const WIRE_PREFIX: &str = "# pcb:wire";

/// Attribute key on port instances listing the physical pad/pin numbers.
const PADS_ATTR: &str = "pads";

/// One endpoint of a persisted wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireEndpoint {
    /// A component pin: `<component_key>@<pin_number>`.
    ///
    /// `key` is the instance path relative to the root (same form as
    /// `# pcb:sch` component comment keys, e.g. `R_PULLUP.R` or
    /// `J15.2309413-1@U1`); `pin` is the KiCad pin number.
    ComponentPin { key: String, pin: String },
    /// A net-symbol endpoint: `<NET>.<index>` (e.g. `GND.2`), matching the
    /// `# pcb:sch` net-symbol comment key form.
    NetSymbol { key: String },
}

impl WireEndpoint {
    /// Parse an endpoint ref token. Tokens are whitespace-free by construction.
    pub fn parse(token: &str) -> Option<Self> {
        if token.chars().any(char::is_whitespace) || token.is_empty() {
            return None;
        }
        if let Some((key, pin)) = token.rsplit_once('@') {
            if key.is_empty() || pin.is_empty() {
                return None;
            }
            return Some(WireEndpoint::ComponentPin {
                key: key.to_string(),
                pin: pin.to_string(),
            });
        }
        // Net-symbol endpoint: `<NET>.<index>` with a numeric index.
        let (net, index) = token.rsplit_once('.')?;
        if net.is_empty() || index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(WireEndpoint::NetSymbol {
            key: token.to_string(),
        })
    }
}

impl std::fmt::Display for WireEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireEndpoint::ComponentPin { key, pin } => write!(f, "{key}@{pin}"),
            WireEndpoint::NetSymbol { key } => f.write_str(key),
        }
    }
}

/// A single persisted wire polyline on a net.
///
/// Points are in the `# pcb:sch` coordinate convention: 0.1 mm units, Y-down.
/// The first point corresponds to `ep_a` and the last point to `ep_b`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersistedWire {
    pub net: String,
    pub ep_a: WireEndpoint,
    pub ep_b: WireEndpoint,
    pub points_01mm: Vec<(f64, f64)>,
}

/// The parsed wire block of a `.zen` file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireBlock {
    /// Block version (always [`WIRE_VERSION`] for blocks this writer emits).
    pub version: u32,
    /// Nethash from the `# pcb:wire-meta` header. Empty when the block had no
    /// valid v1 header (consumers must then treat every wire as unverified).
    #[serde(default)]
    pub nethash: String,
    /// Successfully parsed v1 wires.
    #[serde(default)]
    pub wires: Vec<PersistedWire>,
    /// `# pcb:wire*` lines that were unparseable or of an unknown version.
    /// Preserved verbatim and re-emitted on save; never treated as persisted
    /// wires.
    #[serde(default)]
    pub preserved_lines: Vec<String>,
}

impl Default for WireBlock {
    fn default() -> Self {
        Self {
            version: WIRE_VERSION,
            nethash: String::new(),
            wires: Vec::new(),
            preserved_lines: Vec::new(),
        }
    }
}

impl WireBlock {
    pub fn is_empty(&self) -> bool {
        self.wires.is_empty() && self.preserved_lines.is_empty()
    }
}

// ============================================================================
// Line-level parsing
// ============================================================================

fn parse_points(token: &str) -> Option<Vec<(f64, f64)>> {
    let mut points = Vec::new();
    for pair in token.split(';') {
        let (x, y) = pair.split_once(',')?;
        let x = x.parse::<f64>().ok()?;
        let y = y.parse::<f64>().ok()?;
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        points.push((x, y));
    }
    (points.len() >= 2).then_some(points)
}

fn format_points(points: &[(f64, f64)]) -> String {
    points
        .iter()
        .map(|(x, y)| format!("{x:.4},{y:.4}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// Parse the remainder of a wire line after the `# pcb:wire ` prefix.
fn parse_wire_remainder(remainder: &str) -> Option<PersistedWire> {
    let mut parts = remainder.split_whitespace();
    let version = parts.next()?;
    if version != "v1" {
        return None;
    }
    let net = parts.next()?.to_string();
    let ep_a = WireEndpoint::parse(parts.next()?)?;
    let ep_b = WireEndpoint::parse(parts.next()?)?;
    let points_01mm = parse_points(parts.next()?)?;
    // No trailing tokens allowed.
    if parts.next().is_some() {
        return None;
    }
    Some(PersistedWire {
        net,
        ep_a,
        ep_b,
        points_01mm,
    })
}

/// Parse the remainder of a meta line after the `# pcb:wire-meta` prefix.
/// Returns the nethash on success.
fn parse_meta_remainder(remainder: &str) -> Option<String> {
    let mut parts = remainder.split_whitespace();
    if parts.next()? != "v1" {
        return None;
    }
    let nethash = parts.next()?.strip_prefix("nethash=")?;
    if parts.next().is_some() {
        return None;
    }
    let valid = nethash.len() == 16
        && nethash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    valid.then(|| nethash.to_string())
}

/// Format a single wire line (no trailing newline).
pub fn format_wire_line(wire: &PersistedWire) -> String {
    format!(
        "# pcb:wire v{} {} {} {} {}",
        WIRE_VERSION,
        wire.net,
        wire.ep_a,
        wire.ep_b,
        format_points(&wire.points_01mm)
    )
}

/// Format the wire block header line (no trailing newline).
pub fn format_wire_meta_line(nethash: &str) -> String {
    format!("{WIRE_META_PREFIX} v{WIRE_VERSION} nethash={nethash}")
}

// ============================================================================
// Block-level parsing
// ============================================================================

/// Extract all `# pcb:wire*` lines (trimmed, verbatim otherwise) that appear
/// above the trailing `# pcb:sch` position block.
///
/// Any non-`# pcb:sch` line is above the position block by construction of the
/// bottom-up position parser, so this simply scans the region before the block
/// start. Other `# pcb:*` prefixes are left untouched.
pub fn extract_wire_lines(content: &str) -> Vec<String> {
    let (_, block_start) = crate::position::parse_position_comments(content);
    content[..block_start]
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with(WIRE_PREFIX))
        .map(str::to_string)
        .collect()
}

/// Parse a wire block from pre-extracted `# pcb:wire*` lines.
///
/// Returns `None` when there are no wire lines at all. Tolerance rules:
///
/// * The first valid v1 `# pcb:wire-meta` line provides the nethash; further
///   meta lines (or unknown-version/malformed ones) are preserved verbatim.
/// * Wire lines that fail to parse (or carry an unknown version) are preserved
///   verbatim and never treated as persisted wires.
/// * If no valid v1 header is present, parsed wires are still returned but the
///   nethash is empty, which downstream consumers must treat as unverifiable
///   (demoting every wire).
pub fn parse_wire_block_from_lines<'a>(
    lines: impl IntoIterator<Item = &'a str>,
) -> Option<WireBlock> {
    let mut saw_any = false;
    let mut nethash: Option<String> = None;
    let mut wires = Vec::new();
    let mut preserved_lines = Vec::new();

    for raw in lines {
        let line = raw.trim();
        if !line.starts_with(WIRE_PREFIX) {
            continue;
        }
        saw_any = true;
        if let Some(remainder) = line.strip_prefix(WIRE_META_PREFIX) {
            match parse_meta_remainder(remainder) {
                Some(hash) if nethash.is_none() => nethash = Some(hash),
                _ => {
                    log::warn!("Preserving unrecognized pcb:wire-meta comment: {line}");
                    preserved_lines.push(line.to_string());
                }
            }
        } else if let Some(wire) = line
            .strip_prefix("# pcb:wire ")
            .and_then(parse_wire_remainder)
        {
            wires.push(wire);
        } else {
            log::warn!("Preserving unrecognized pcb:wire comment: {line}");
            preserved_lines.push(line.to_string());
        }
    }

    saw_any.then(|| WireBlock {
        version: WIRE_VERSION,
        nethash: nethash.unwrap_or_default(),
        wires,
        preserved_lines,
    })
}

/// Parse the wire block from full `.zen` file content.
///
/// Returns `None` when the file has no `# pcb:wire*` lines.
pub fn parse_wire_comments(content: &str) -> Option<WireBlock> {
    let lines = extract_wire_lines(content);
    parse_wire_block_from_lines(lines.iter().map(String::as_str))
}

/// Serialize a wire block to comment text (each line newline-terminated).
///
/// The header and wire lines are emitted only when at least one parsed wire is
/// present (an empty survivor set writes no v1 block); preserved lines are
/// always re-emitted verbatim. Wire lines are sorted for deterministic output.
pub fn format_wire_block(block: &WireBlock) -> String {
    let mut out = String::new();
    if !block.wires.is_empty() {
        out.push_str(&format_wire_meta_line(&block.nethash));
        out.push('\n');
        let mut lines: Vec<String> = block.wires.iter().map(format_wire_line).collect();
        lines.sort_by(|a, b| natord::compare(a, b));
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
    }
    for line in &block.preserved_lines {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Rewrite the wire block within full `.zen` file content.
///
/// Strips every existing `# pcb:wire*` line above the `# pcb:sch` block, then
/// inserts the formatted `block` directly above the position block. The
/// position block itself is preserved byte-for-byte.
pub fn update_wire_comments(content: &str, block: &WireBlock) -> String {
    let (_, block_start) = crate::position::parse_position_comments(content);
    let before = &content[..block_start];
    let after = &content[block_start..];

    // Strip existing wire lines wherever they appear above the position block.
    let mut stripped = String::with_capacity(before.len());
    for line in before.split_inclusive('\n') {
        if line.trim().starts_with(WIRE_PREFIX) {
            continue;
        }
        stripped.push_str(line);
    }

    let new_block = format_wire_block(block);
    if new_block.is_empty() {
        return format!("{stripped}{after}");
    }

    // The wire block must start on its own line, directly above the position
    // block (the bottom-up position parser stops at the first wire line, so
    // everything above — including this block — survives old-editor saves).
    if !stripped.is_empty() && !stripped.ends_with('\n') {
        stripped.push('\n');
    }
    format!("{stripped}{new_block}{after}")
}

/// Rewrite the wire block of a `.zen` file on disk.
///
/// No-op (no write) when the update leaves the content unchanged.
pub fn replace_wire_comments<P: AsRef<Path>>(file_path: P, block: &WireBlock) -> std::io::Result<()> {
    let content = std::fs::read_to_string(&file_path)?;
    let updated = update_wire_comments(&content, block);
    if updated != content {
        std::fs::write(&file_path, updated)?;
    }
    Ok(())
}

// ============================================================================
// Nethash
// ============================================================================

/// Compute the canonical netlist fingerprint from `(net_name, pin_ref)` pairs,
/// where `pin_ref` is `<component_key>.<pin_number>`.
///
/// The hash is the first 16 lowercase hex chars of the SHA-256 over the sorted
/// `<net_name>\t<pin_ref>` lines joined with `\n`. Input order is irrelevant.
pub fn compute_nethash(entries: impl IntoIterator<Item = (String, String)>) -> String {
    let mut lines: Vec<String> = entries
        .into_iter()
        .map(|(net, pin_ref)| format!("{net}\t{pin_ref}"))
        .collect();
    lines.sort();
    lines.dedup();

    let mut hasher = Sha256::new();
    hasher.update(lines.join("\n").as_bytes());
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Compute the nethash of a compiled [`Schematic`]: one entry per component
/// pin, keyed by the instance path relative to the root.
///
/// Pins are expanded from each net port's `pads` attribute; ports without pad
/// metadata fall back to the logical port name so the hash stays total.
pub fn compute_schematic_nethash(schematic: &Schematic) -> String {
    let mut entries = Vec::new();
    for (net_name, net) in &schematic.nets {
        for port_ref in &net.ports {
            let Some((comp_ref, port_name)) = schematic.component_ref_and_pin_for_port(port_ref)
            else {
                continue;
            };
            let comp_key = comp_ref.instance_path.join(".");
            let pads = schematic
                .instances
                .get(port_ref)
                .map(|inst| inst.string_list_attr(&[PADS_ATTR]))
                .unwrap_or_default();
            if pads.is_empty() {
                entries.push((net_name.clone(), format!("{comp_key}.{port_name}")));
            } else {
                for pad in pads {
                    entries.push((net_name.clone(), format!("{comp_key}.{pad}")));
                }
            }
        }
    }
    compute_nethash(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::{parse_position_comments, update_position_comments};
    use std::collections::BTreeMap;

    fn wire(net: &str, ep_a: &str, ep_b: &str, points: &[(f64, f64)]) -> PersistedWire {
        PersistedWire {
            net: net.to_string(),
            ep_a: WireEndpoint::parse(ep_a).expect("valid endpoint"),
            ep_b: WireEndpoint::parse(ep_b).expect("valid endpoint"),
            points_01mm: points.to_vec(),
        }
    }

    const CONTENT_WITH_WIRES: &str = r#"load("@stdlib/interfaces.zen", "Power")

Resistor("R_PULLUP", "10kOhm", "0603", P1=vcc.NET, P2=gnd.NET)

# pcb:wire-meta v1 nethash=0123456789abcdef
# pcb:wire v1 GND R_PULLUP.R@2 GND.0 723.9000,88.9000;723.9000,101.6000
# pcb:wire v1 VCC R_PULLUP.R@1 VCC.1 723.9000,63.5000;700.0000,63.5000;700.0000,50.8000
# pcb:sch GND.0 x=723.9000 y=101.6000 rot=0
# pcb:sch R_PULLUP.R x=723.9000 y=88.9000 rot=180
# pcb:sch VCC.1 x=700.0000 y=50.8000 rot=0
"#;

    // ------------------------------------------------------------------
    // Endpoint grammar
    // ------------------------------------------------------------------

    #[test]
    fn endpoint_parse_component_pin() {
        assert_eq!(
            WireEndpoint::parse("R_PULLUP.R@2"),
            Some(WireEndpoint::ComponentPin {
                key: "R_PULLUP.R".to_string(),
                pin: "2".to_string(),
            })
        );
        // Multi-unit component keys keep their @U suffix in the key.
        assert_eq!(
            WireEndpoint::parse("J15.2309413-1@U1@A7"),
            Some(WireEndpoint::ComponentPin {
                key: "J15.2309413-1@U1".to_string(),
                pin: "A7".to_string(),
            })
        );
    }

    #[test]
    fn endpoint_parse_net_symbol() {
        assert_eq!(
            WireEndpoint::parse("GND.2"),
            Some(WireEndpoint::NetSymbol {
                key: "GND.2".to_string()
            })
        );
        // Dotted (scoped) net names keep the full key.
        assert_eq!(
            WireEndpoint::parse("sub.GND.0"),
            Some(WireEndpoint::NetSymbol {
                key: "sub.GND.0".to_string()
            })
        );
    }

    #[test]
    fn endpoint_parse_rejects_garbage() {
        assert_eq!(WireEndpoint::parse(""), None);
        assert_eq!(WireEndpoint::parse("GND"), None); // no index, no pin
        assert_eq!(WireEndpoint::parse("GND.x"), None); // non-numeric index
        assert_eq!(WireEndpoint::parse("@2"), None); // empty component key
        assert_eq!(WireEndpoint::parse("R1@"), None); // empty pin
    }

    #[test]
    fn endpoint_display_round_trips() {
        for token in ["R_PULLUP.R@2", "J15.2309413-1@U1@A7", "GND.2", "sub.GND.0"] {
            let ep = WireEndpoint::parse(token).expect("valid");
            assert_eq!(ep.to_string(), token);
            assert_eq!(WireEndpoint::parse(&ep.to_string()), Some(ep));
        }
    }

    // ------------------------------------------------------------------
    // Block parse + round trip
    // ------------------------------------------------------------------

    #[test]
    fn parse_wire_comments_reads_block() {
        let block = parse_wire_comments(CONTENT_WITH_WIRES).expect("block present");
        assert_eq!(block.version, WIRE_VERSION);
        assert_eq!(block.nethash, "0123456789abcdef");
        assert_eq!(block.wires.len(), 2);
        assert!(block.preserved_lines.is_empty());

        let gnd = &block.wires[0];
        assert_eq!(gnd.net, "GND");
        assert_eq!(
            gnd.ep_a,
            WireEndpoint::ComponentPin {
                key: "R_PULLUP.R".to_string(),
                pin: "2".to_string()
            }
        );
        assert_eq!(
            gnd.ep_b,
            WireEndpoint::NetSymbol {
                key: "GND.0".to_string()
            }
        );
        assert_eq!(gnd.points_01mm, vec![(723.9, 88.9), (723.9, 101.6)]);

        assert_eq!(block.wires[1].points_01mm.len(), 3);
    }

    #[test]
    fn parse_returns_none_without_wire_lines() {
        assert_eq!(
            parse_wire_comments("load(\"x\")\n\n# pcb:sch A x=1.0 y=2.0 rot=0\n"),
            None
        );
        assert_eq!(parse_wire_comments(""), None);
    }

    #[test]
    fn block_round_trips_through_format_and_parse() {
        let block = parse_wire_comments(CONTENT_WITH_WIRES).expect("block");
        let text = format_wire_block(&block);
        let reparsed =
            parse_wire_block_from_lines(text.lines()).expect("formatted block re-parses");
        assert_eq!(reparsed, block);
    }

    #[test]
    fn format_is_deterministic_regardless_of_wire_order() {
        let a = wire("GND", "R1.R@2", "GND.0", &[(0.0, 0.0), (10.0, 0.0)]);
        let b = wire("VCC", "R1.R@1", "VCC.0", &[(0.0, 5.0), (10.0, 5.0)]);
        let block_ab = WireBlock {
            nethash: "0123456789abcdef".to_string(),
            wires: vec![a.clone(), b.clone()],
            ..Default::default()
        };
        let block_ba = WireBlock {
            nethash: "0123456789abcdef".to_string(),
            wires: vec![b, a],
            ..Default::default()
        };
        assert_eq!(format_wire_block(&block_ab), format_wire_block(&block_ba));
    }

    // ------------------------------------------------------------------
    // Tolerance rules (P1)
    // ------------------------------------------------------------------

    #[test]
    fn unknown_version_wire_line_is_preserved_not_parsed() {
        let content = "\
# pcb:wire-meta v1 nethash=0123456789abcdef
# pcb:wire v2 GND R1.R@2 GND.0 0.0000,0.0000;10.0000,0.0000 extra
# pcb:wire v1 VCC R1.R@1 VCC.0 0.0000,5.0000;10.0000,5.0000
# pcb:sch R1.R x=1.0 y=2.0 rot=0
";
        let block = parse_wire_comments(content).expect("block");
        assert_eq!(block.wires.len(), 1, "v2 line must not be treated as a wire");
        assert_eq!(block.wires[0].net, "VCC");
        assert_eq!(
            block.preserved_lines,
            vec!["# pcb:wire v2 GND R1.R@2 GND.0 0.0000,0.0000;10.0000,0.0000 extra".to_string()]
        );

        // A save re-emits the preserved line verbatim (never stripped).
        let text = format_wire_block(&block);
        assert!(text.contains("# pcb:wire v2 GND"));
    }

    #[test]
    fn unknown_version_meta_line_is_preserved_and_hash_unverified() {
        let content = "\
# pcb:wire-meta v2 nethash=0123456789abcdef flags=zzz
# pcb:wire v1 GND R1.R@2 GND.0 0.0000,0.0000;10.0000,0.0000
# pcb:sch R1.R x=1.0 y=2.0 rot=0
";
        let block = parse_wire_comments(content).expect("block");
        assert_eq!(block.nethash, "", "v2 header must not provide a nethash");
        assert_eq!(block.wires.len(), 1);
        assert_eq!(block.preserved_lines.len(), 1);
    }

    #[test]
    fn malformed_wire_lines_are_preserved() {
        let malformed = [
            "# pcb:wire v1 GND R1.R@2 GND.0",                        // missing points
            "# pcb:wire v1 GND R1.R@2 GND.0 0.0,0.0",                // single point
            "# pcb:wire v1 GND R1.R@2 GND.0 nonsense",               // bad points
            "# pcb:wire v1 GND bogus GND.0 0.0,0.0;1.0,1.0",         // bad endpoint
            "# pcb:wire v1 GND R1.R@2 GND.0 0.0,0.0;1.0,1.0 junk",   // trailing token
            "# pcb:wire-metadata v1 nethash=0123456789abcdef",       // unknown pcb:wire* form
            "# pcb:wire",                                            // bare prefix
        ];
        let content = format!("{}\n# pcb:sch R1.R x=1.0 y=2.0 rot=0\n", malformed.join("\n"));
        let block = parse_wire_comments(&content).expect("block");
        assert!(block.wires.is_empty());
        assert_eq!(block.preserved_lines.len(), malformed.len());
        for line in malformed {
            assert!(block.preserved_lines.contains(&line.to_string()));
        }
    }

    #[test]
    fn second_meta_line_is_preserved_first_wins() {
        let content = "\
# pcb:wire-meta v1 nethash=0123456789abcdef
# pcb:wire-meta v1 nethash=fedcba9876543210
# pcb:sch R1.R x=1.0 y=2.0 rot=0
";
        let block = parse_wire_comments(content).expect("block");
        assert_eq!(block.nethash, "0123456789abcdef");
        assert_eq!(
            block.preserved_lines,
            vec!["# pcb:wire-meta v1 nethash=fedcba9876543210".to_string()]
        );
    }

    #[test]
    fn missing_meta_header_yields_empty_nethash() {
        let content = "\
# pcb:wire v1 GND R1.R@2 GND.0 0.0000,0.0000;10.0000,0.0000
# pcb:sch R1.R x=1.0 y=2.0 rot=0
";
        let block = parse_wire_comments(content).expect("block");
        assert_eq!(block.nethash, "");
        assert_eq!(block.wires.len(), 1);
    }

    #[test]
    fn invalid_nethash_forms_are_rejected() {
        for bad in [
            "# pcb:wire-meta v1 nethash=0123",                 // too short
            "# pcb:wire-meta v1 nethash=0123456789ABCDEF",     // uppercase
            "# pcb:wire-meta v1 nethash=0123456789abcdefff",   // too long
            "# pcb:wire-meta v1 hash=0123456789abcdef",        // wrong key
            "# pcb:wire-meta v1 nethash=0123456789abcdef x=1", // trailing token
        ] {
            let content = format!("{bad}\n# pcb:sch R1.R x=1.0 y=2.0 rot=0\n");
            let block = parse_wire_comments(&content).expect("block");
            assert_eq!(block.nethash, "", "{bad} must not provide a nethash");
            assert_eq!(block.preserved_lines, vec![bad.to_string()]);
        }
    }

    // ------------------------------------------------------------------
    // Interplay with the # pcb:sch position block (old-editor safety)
    // ------------------------------------------------------------------

    #[test]
    fn wire_block_does_not_disturb_position_parsing() {
        let (positions, block_start) = parse_position_comments(CONTENT_WITH_WIRES);
        assert_eq!(positions.len(), 3, "all positions parse with wires above");
        assert!(positions.contains_key("R_PULLUP.R"));
        assert!(positions.contains_key("GND.0"));
        assert!(positions.contains_key("VCC.1"));
        // The position block starts below the wire lines.
        assert!(!CONTENT_WITH_WIRES[block_start..].contains("pcb:wire"));
    }

    #[test]
    fn old_editor_position_save_preserves_wire_lines_verbatim() {
        // Simulate an old (wire-unaware) editor saving positions: it truncates
        // from the position block start and re-emits merged positions. Wire
        // lines above the block must survive byte-for-byte.
        let mut new_positions = BTreeMap::new();
        new_positions.insert(
            "R_PULLUP.R".to_string(),
            crate::position::Position {
                x: 500.0,
                y: 600.0,
                rotation: 90.0,
                mirror: None,
            },
        );
        let (truncate_pos, position_comments) =
            update_position_comments(CONTENT_WITH_WIRES, &new_positions);
        let updated = format!(
            "{}{}",
            &CONTENT_WITH_WIRES[..truncate_pos],
            position_comments
        );

        assert!(updated.contains("# pcb:wire-meta v1 nethash=0123456789abcdef"));
        assert!(updated.contains(
            "# pcb:wire v1 GND R_PULLUP.R@2 GND.0 723.9000,88.9000;723.9000,101.6000"
        ));
        assert!(updated.contains(
            "# pcb:wire v1 VCC R_PULLUP.R@1 VCC.1 723.9000,63.5000;700.0000,63.5000;700.0000,50.8000"
        ));
        // Position content updated as usual.
        assert!(updated.contains("# pcb:sch R_PULLUP.R x=500.0000 y=600.0000 rot=90"));
        assert!(updated.contains("# pcb:sch GND.0"));
        assert!(updated.contains("# pcb:sch VCC.1"));

        // And the saved file still parses the same wire block afterwards.
        let block = parse_wire_comments(&updated).expect("block survives");
        assert_eq!(block.wires.len(), 2);
        assert_eq!(block.nethash, "0123456789abcdef");
    }

    // ------------------------------------------------------------------
    // update_wire_comments (new-editor save)
    // ------------------------------------------------------------------

    #[test]
    fn update_wire_comments_rewrites_block_above_positions() {
        let block = WireBlock {
            nethash: "fedcba9876543210".to_string(),
            wires: vec![wire("GND", "R_PULLUP.R@2", "GND.0", &[(1.0, 2.0), (3.0, 4.0)])],
            ..Default::default()
        };
        let updated = update_wire_comments(CONTENT_WITH_WIRES, &block);

        // Old wire lines stripped, new block present.
        assert!(!updated.contains("nethash=0123456789abcdef"));
        assert!(!updated.contains("723.9000,63.5000"));
        assert!(updated.contains("# pcb:wire-meta v1 nethash=fedcba9876543210"));
        assert!(updated.contains("# pcb:wire v1 GND R_PULLUP.R@2 GND.0 1.0000,2.0000;3.0000,4.0000"));

        // Position block untouched.
        assert!(updated.contains("# pcb:sch R_PULLUP.R x=723.9000 y=88.9000 rot=180"));
        let (positions, _) = parse_position_comments(&updated);
        assert_eq!(positions.len(), 3);

        // Wire block sits directly above the position block.
        let wire_pos = updated.find("# pcb:wire-meta").unwrap();
        let sch_pos = updated.find("# pcb:sch").unwrap();
        assert!(wire_pos < sch_pos);

        // Round trip: parsing the updated file returns the same block.
        assert_eq!(parse_wire_comments(&updated), Some(block));
    }

    #[test]
    fn update_wire_comments_with_empty_block_strips_everything() {
        let updated = update_wire_comments(CONTENT_WITH_WIRES, &WireBlock::default());
        assert!(!updated.contains("pcb:wire"));
        let (positions, _) = parse_position_comments(&updated);
        assert_eq!(positions.len(), 3);
        assert!(updated.contains("Resistor(\"R_PULLUP\""));
    }

    #[test]
    fn update_wire_comments_reemits_preserved_lines_without_header_when_no_wires() {
        let block = WireBlock {
            preserved_lines: vec!["# pcb:wire v2 GND future stuff".to_string()],
            ..Default::default()
        };
        let updated = update_wire_comments(CONTENT_WITH_WIRES, &block);
        assert!(updated.contains("# pcb:wire v2 GND future stuff"));
        assert!(
            !updated.contains("# pcb:wire-meta"),
            "no v1 header when zero v1 wires survive"
        );
    }

    #[test]
    fn update_wire_comments_leaves_unknown_pcb_prefixes_alone() {
        let content = "\
code()

# pcb:future v3 something
# pcb:wire v1 GND R1.R@2 GND.0 0.0000,0.0000;1.0000,1.0000
# pcb:sch R1.R x=1.0 y=2.0 rot=0
";
        let updated = update_wire_comments(content, &WireBlock::default());
        assert!(updated.contains("# pcb:future v3 something"));
        assert!(!updated.contains("pcb:wire"));
    }

    #[test]
    fn update_wire_comments_strips_wire_lines_scattered_above_block() {
        let content = "\
code()
# pcb:wire v1 STALE R1.R@1 STALE.0 0.0000,0.0000;1.0000,1.0000
more_code()

# pcb:wire-meta v1 nethash=0123456789abcdef
# pcb:sch R1.R x=1.0 y=2.0 rot=0
";
        let updated = update_wire_comments(content, &WireBlock::default());
        assert!(!updated.contains("pcb:wire"));
        assert!(updated.contains("more_code()"));
    }

    #[test]
    fn update_wire_comments_without_position_block_appends_at_end() {
        let content = "code()\n";
        let block = WireBlock {
            nethash: "0123456789abcdef".to_string(),
            wires: vec![wire("GND", "R1.R@2", "GND.0", &[(0.0, 0.0), (1.0, 1.0)])],
            ..Default::default()
        };
        let updated = update_wire_comments(content, &block);
        assert!(updated.starts_with("code()\n"));
        assert!(updated.contains("# pcb:wire-meta v1 nethash=0123456789abcdef"));
        // Still parses back.
        assert_eq!(parse_wire_comments(&updated).map(|b| b.wires.len()), Some(1));
    }

    #[test]
    fn replace_wire_comments_writes_file() {
        use tempfile::NamedTempFile;
        let temp = NamedTempFile::new().expect("temp file");
        std::fs::write(temp.path(), CONTENT_WITH_WIRES).expect("write");

        let block = WireBlock {
            nethash: "fedcba9876543210".to_string(),
            wires: vec![wire("GND", "R_PULLUP.R@2", "GND.0", &[(1.0, 2.0), (3.0, 4.0)])],
            ..Default::default()
        };
        replace_wire_comments(temp.path(), &block).expect("replace");
        let updated = std::fs::read_to_string(temp.path()).expect("read");
        assert!(updated.contains("nethash=fedcba9876543210"));
        assert!(!updated.contains("nethash=0123456789abcdef"));

        // No-op save leaves content byte-identical.
        replace_wire_comments(temp.path(), &block).expect("replace again");
        assert_eq!(std::fs::read_to_string(temp.path()).expect("read"), updated);
    }

    // ------------------------------------------------------------------
    // Nethash
    // ------------------------------------------------------------------

    #[test]
    fn nethash_is_order_independent_and_16_lowercase_hex() {
        let entries = |order: bool| {
            let mut v = vec![
                ("GND".to_string(), "R1.R.2".to_string()),
                ("VCC".to_string(), "R1.R.1".to_string()),
            ];
            if order {
                v.reverse();
            }
            v
        };
        let a = compute_nethash(entries(false));
        let b = compute_nethash(entries(true));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
    }

    #[test]
    fn nethash_changes_when_partition_changes() {
        let base = compute_nethash(vec![("GND".to_string(), "R1.R.2".to_string())]);
        let renamed = compute_nethash(vec![("AGND".to_string(), "R1.R.2".to_string())]);
        let moved = compute_nethash(vec![("GND".to_string(), "R2.R.2".to_string())]);
        assert_ne!(base, renamed);
        assert_ne!(base, moved);
    }

    // ------------------------------------------------------------------
    // Serde compatibility
    // ------------------------------------------------------------------

    #[test]
    fn wire_block_serde_round_trip() {
        let block = WireBlock {
            nethash: "0123456789abcdef".to_string(),
            wires: vec![wire("GND", "R1.R@2", "GND.0", &[(0.0, 0.0), (1.0, 1.0)])],
            preserved_lines: vec!["# pcb:wire v2 future".to_string()],
            ..Default::default()
        };
        let json = serde_json::to_string(&block).expect("serialize");
        let back: WireBlock = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, block);
    }

    #[test]
    fn instance_without_wire_block_field_deserializes() {
        // Old compiled netlist JSON has no `wire_block` key on instances.
        let json = r#"{
            "type_ref": {"source_path": "/tmp/a.zen", "module_name": "A"},
            "kind": "Module",
            "attributes": {},
            "children": {},
            "reference_designator": null
        }"#;
        let instance: crate::Instance = serde_json::from_str(json).expect("old JSON deserializes");
        assert!(instance.wire_block.is_none());
    }
}
