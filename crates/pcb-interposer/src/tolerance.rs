//! Hole tolerances in the interposer's manufacturing IPC-2581.
//!
//! KiCad exports every hole with a zero tolerance, and a drill file
//! carries none at all. [`hold`] writes each alignment feature's
//! tolerance onto its hole or slot in the exported file, where the fab
//! drawing reads it back as a note.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::bed::Bed;

/// The value of `name` in an XML start tag.
fn attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    Some(&tag[start..start + tag[start..].find('"')?])
}

fn set_attr(tag: &str, name: &str, value: f64) -> Option<String> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = start + tag[start..].find('"')?;
    Some(format!("{}{value}{}", &tag[..start], &tag[end..]))
}

/// Write `bed`'s alignment tolerances into the IPC-2581 file at `path`,
/// which KiCad exported from the interposer board. Returns how many
/// features took one.
pub fn hold(path: &Path, bed: Bed) -> Result<usize> {
    let held: Vec<_> = bed
        .alignment()
        .into_iter()
        .filter(|alignment| alignment.tolerance != [0.0, 0.0])
        .collect();
    if held.is_empty() {
        return Ok(0);
    }
    let source =
        std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let mut lines: Vec<String> = source.lines().map(str::to_string).collect();
    // KiCad writes board coordinates with y up; the board's are y down.
    let tolerance_at = |tag: &str| -> Option<[f64; 2]> {
        let x: f64 = attr(tag, "x")?.parse().ok()?;
        let y: f64 = attr(tag, "y")?.parse().ok()?;
        held.iter()
            .find(|a| (a.at[0] - x).abs() < 1e-3 && (a.at[1] + y).abs() < 1e-3)
            .map(|a| a.tolerance)
    };
    let stamp = |line: &str, [plus, minus]: [f64; 2]| -> Option<String> {
        set_attr(&set_attr(line, "plusTol", plus)?, "minusTol", minus)
    };
    let mut count = 0;
    let mut slot = None;
    for index in 0..lines.len() {
        let line = lines[index].trim_start();
        if line.starts_with("<Hole ") {
            if let Some(tolerance) = tolerance_at(line) {
                lines[index] = stamp(&lines[index], tolerance)
                    .context("an exported hole states no tolerance to replace")?;
                count += 1;
            }
        } else if line.starts_with("<SlotCavity ") {
            slot = Some(index);
        } else if line.starts_with("<Location ") {
            // A slot's position is its first child.
            if let Some((opening, tolerance)) = slot.take().zip(tolerance_at(line)) {
                lines[opening] = stamp(&lines[opening], tolerance)
                    .context("an exported slot states no tolerance to replace")?;
                count += 1;
            }
        } else {
            slot = None;
        }
    }
    if count != held.len() {
        bail!(
            "{} of the bed's {} toleranced alignment holes are in {}",
            count,
            held.len(),
            path.display()
        );
    }
    let mut out = lines.join("\n");
    if source.ends_with('\n') {
        out.push('\n');
    }
    std::fs::write(path, out).with_context(|| format!("write {}", path.display()))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dowel_hole_and_slot_take_their_tolerance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.xml");
        let xml = r#"<Set>
  <Hole name="H1" diameter="3.050" platingStatus="NONPLATED" plusTol="0.0" minusTol="0.0" x="28.50" y="-31.250"/>
  <Hole name="H2" diameter="3.40" platingStatus="NONPLATED" plusTol="0.0" minusTol="0.0" x="52.50" y="-31.250"/>
  <SlotCavity name="SLOT1" platingStatus="NONPLATED" plusTol="0.0" minusTol="0.0">
    <Location x="76.50" y="-31.250"/>
    <Oval width="4.0" height="3.050"/>
  </SlotCavity>
</Set>
"#;
        std::fs::write(&path, xml).unwrap();
        assert_eq!(hold(&path, Bed::SwitchyardRev1).unwrap(), 2);
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains(
            r#"name="H1" diameter="3.050" platingStatus="NONPLATED" plusTol="0.05" minusTol="0" x"#
        ));
        assert!(out.contains(
            r#"name="H2" diameter="3.40" platingStatus="NONPLATED" plusTol="0.0" minusTol="0.0""#
        ));
        assert!(
            out.contains(r#"name="SLOT1" platingStatus="NONPLATED" plusTol="0.05" minusTol="0">"#)
        );
        assert_eq!(hold(&path, Bed::S13).unwrap(), 0);

        // A file without the dowel holes is not the bed's interposer.
        std::fs::write(&path, "<Set/>\n").unwrap();
        assert!(hold(&path, Bed::SwitchyardRev1).is_err());
    }
}
