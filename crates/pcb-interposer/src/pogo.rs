//! The pogo-pin footprint, stamped from a vendored `.kicad_mod` template.
//!
//! The pogo pin is a real, purchasable part, so its footprint comes from a
//! KiCad footprint file rather than this crate's generated primitives —
//! that file is where the land geometry, courtyard, BOM properties
//! (MPN/Manufacturer), and the embedded STEP model live. Swapping in the
//! selected part is a file replacement: drop in its `.kicad_mod` (with
//! the 3D model embedded via KiCad's embedded-files support) and every
//! generated interposer carries it.
//!
//! Stamping keeps the template text verbatim — embedded binary blocks
//! round-trip untouched — and rewrites only the instance fields: library
//! id, position, reference, the pad nets, and deterministic UUIDs.

use anyhow::{Context, Result, bail};

/// The vendored template, compiled in.
const TEMPLATE: &str = include_str!("../footprints/Pogo_Pad_D1.0mm.kicad_mod");
const LIB_NICKNAME: &str = "Interposer";

/// A parsed-and-validated stamping template.
pub struct PogoTemplate {
    /// Text after the file-only header lines, starting at `(layer`.
    body: String,
    /// In-board footprint header, `(footprint "Interposer:Name"`.
    header: String,
}

impl PogoTemplate {
    /// Load and validate the vendored template.
    pub fn load() -> Result<Self> {
        Self::from_source(TEMPLATE)
    }

    fn from_source(source: &str) -> Result<Self> {
        let mut lines = source.lines();
        let first = lines.next().context("empty footprint template")?;
        let name = first
            .strip_prefix("(footprint \"")
            .and_then(|rest| rest.strip_suffix('"'))
            .with_context(|| format!("template must open with (footprint \"name\"): {first}"))?;
        // Drop the standalone-file header (version/generator*) — a board
        // instance carries the document's header instead.
        let body: String = source
            .lines()
            .skip(1)
            .filter(|line| {
                let t = line.trim_start();
                !(t.starts_with("(version ") || t.starts_with("(generator"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        for anchor in ["(property \"Reference\" \"REF**\"", "(pad "] {
            if !body.contains(anchor) {
                bail!("footprint template is missing the {anchor:?} anchor");
            }
        }
        Ok(Self {
            body,
            header: format!("(footprint \"{LIB_NICKNAME}:{name}\""),
        })
    }

    /// Stamp one board instance: position, reference, every pad bound to
    /// `net`, and fresh deterministic UUIDs drawn from `uuids`.
    pub fn stamp(
        &self,
        at: [f64; 2],
        reference: &str,
        net: (u32, String),
        uuids: &mut pcb_ir::dialects::kicad::UuidGen,
    ) -> String {
        let mut out = String::with_capacity(self.body.len() + 256);
        out.push_str(&self.header);
        out.push('\n');
        out.push_str(&format!("\t(uuid \"{}\")\n", uuids.next_uuid()));
        out.push_str(&format!("\t(at {} {})\n", fmt(at[0]), fmt(at[1])));
        let body = self.body.replace(
            "(property \"Reference\" \"REF**\"",
            &format!("(property \"Reference\" \"{reference}\""),
        );
        // Bind every pad: KiCad wants the net right after the layer list.
        let body = body.replace(
            "(layers \"F.Cu\" \"F.Mask\")",
            &format!(
                "(layers \"F.Cu\" \"F.Mask\")\n\t\t(net {} \"{}\")",
                net.0, net.1
            ),
        );
        // Fresh instance UUIDs.
        for (index, piece) in body.split("(uuid \"").enumerate() {
            if index == 0 {
                out.push_str(piece);
                continue;
            }
            let rest = piece.split_once('"').map(|(_, rest)| rest).unwrap_or("");
            out.push_str("(uuid \"");
            out.push_str(&uuids.next_uuid());
            out.push('"');
            out.push_str(rest);
        }
        out
    }
}

fn fmt(value: f64) -> String {
    let formatted = format!("{value:.4}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_ir::dialects::kicad::UuidGen;

    #[test]
    fn stamps_a_bound_instance() {
        let template = PogoTemplate::load().expect("vendored template is valid");
        let mut uuids = UuidGen::new();
        let text = template.stamp([12.5, 30.0], "P7", (3, "B0.TP_X.TP".into()), &mut uuids);

        assert!(text.starts_with("(footprint \"Interposer:Pogo_Pad_D1.0mm\"\n"));
        assert!(text.contains("(at 12.5 30)"));
        assert!(text.contains("(property \"Reference\" \"P7\""));
        assert!(text.contains("(net 3 \"B0.TP_X.TP\")"));
        // File-only header lines are stripped; template UUIDs are replaced.
        assert!(!text.contains("(version"));
        assert!(!text.contains("(generator"));
        assert!(!text.contains("0000000000a1"));

        // Deterministic given the same generator state.
        let again = template.stamp(
            [12.5, 30.0],
            "P7",
            (3, "B0.TP_X.TP".into()),
            &mut UuidGen::new(),
        );
        let first = template.stamp(
            [12.5, 30.0],
            "P7",
            (3, "B0.TP_X.TP".into()),
            &mut UuidGen::new(),
        );
        assert_eq!(again, first);
    }
}
