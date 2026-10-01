//! Known defects of the exporters that write our source files, undone once
//! at import so every consumer sees the file the exporter meant to write.

use super::*;

/// A KiCad release whose exporter wrote a flipped footprint's `Xform`
/// rotation in a non-standard form.
///
/// IPC-2581C mirrors X after rotating, so a footprint KiCad reports at
/// orientation `o` on the bottom needs rotation `-o - 180`. KiCad 9.0.8 and
/// 9.0.9 wrote `-o`; 10.0.0 through 10.0.4 wrote `o`. KiCad 10.0.5 restored
/// the standard form (kicad#18013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlippedRotationDefect {
    /// The writing exporter, e.g. `KiCad 10.0.3`.
    pub exporter: String,
    form: FlippedRotationForm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlippedRotationForm {
    /// `-o`: missing the 180° turn.
    Negated,
    /// `o`: missing both the negation and the 180° turn.
    Unadjusted,
}

impl FlippedRotationDefect {
    /// The defect of the exporter that originally wrote `ipc`, if any. Later
    /// tools keep the first `FileRevision`, and none of ours rewrite
    /// component placements.
    pub fn of(ipc: &Ipc2581) -> Option<Self> {
        let package = ipc
            .history_record()?
            .file_revision
            .as_ref()?
            .software_package
            .as_ref()?;
        if ipc.resolve(package.name) != "KiCad" {
            return None;
        }
        let revision = ipc.resolve(package.revision?);
        let version = revision
            .split('.')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        let form = match version.as_slice() {
            [9, 0, 8 | 9, ..] => FlippedRotationForm::Negated,
            [10, 0, 0..=4, ..] => FlippedRotationForm::Unadjusted,
            _ => return None,
        };
        Some(Self {
            exporter: format!("KiCad {revision}"),
            form,
        })
    }

    /// The standard rotation of a flipped footprint this exporter wrote as
    /// `degrees`.
    pub fn standard_rotation(&self, degrees: f64) -> f64 {
        match self.form {
            FlippedRotationForm::Negated => degrees - 180.0,
            FlippedRotationForm::Unadjusted => -degrees - 180.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board with one flipped and one unflipped part, as `revision` wrote it.
    fn board(revision: &str, flipped_rotation: f64) -> Ipc2581 {
        Ipc2581::parse(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<IPC-2581 revision="C" xmlns="http://webstds.ipc.org/2581">
  <Content roleRef="Owner"><FunctionMode mode="ASSEMBLY"/><StepRef name="board"/></Content>
  <HistoryRecord number="1" origination="2026-01-01T00:00:00Z" software="KiCad" lastChange="2026-01-01T00:00:00Z">
    <FileRevision fileRevisionId="1" comment="">
      <SoftwarePackage name="KiCad" revision="{revision}" vendor="KiCad EDA"/>
    </FileRevision>
  </HistoryRecord>
  <Ecad><CadHeader units="MILLIMETER"/><CadData>
    <Layer name="B.Cu" layerFunction="CONDUCTOR" side="BOTTOM"/>
    <Step name="board" type="BOARD">
      <Component refDes="U1" packageRef="QFN" part="ic" layerRef="B.Cu" mountType="SMT">
        <Xform rotation="{flipped_rotation}" mirror="true"/><Location x="10" y="20"/>
      </Component>
      <Component refDes="U2" packageRef="QFN" part="ic" layerRef="B.Cu" mountType="SMT">
        <Xform rotation="30"/><Location x="10" y="20"/>
      </Component>
    </Step>
  </CadData></Ecad>
</IPC-2581>"#
        ))
        .unwrap()
    }

    fn written_by(revision: &str) -> Option<FlippedRotationDefect> {
        FlippedRotationDefect::of(&board(revision, 0.0))
    }

    #[test]
    fn import_places_defective_flipped_parts_in_standard_form() {
        let standard = import_design(&board("10.0.5", -210.0), Resolution::default()).unwrap();
        let defective = import_design(&board("10.0.3", 30.0), Resolution::default()).unwrap();
        assert_eq!(standard.flipped_rotation_defect, None);
        assert_eq!(
            defective.flipped_rotation_defect.as_ref().unwrap().exporter,
            "KiCad 10.0.3"
        );
        let landmark = |design: &ImportedDesign, index: usize| {
            design.components[index]
                .local_from_component
                .transform_point(Point::new(2.0, 1.0))
        };
        for index in [0, 1] {
            let (expected, actual) = (landmark(&standard, index), landmark(&defective, index));
            assert!((expected.x - actual.x).abs() < 1e-9 && (expected.y - actual.y).abs() < 1e-9);
        }
        let corrected = |design: &ImportedDesign| {
            design
                .components
                .iter()
                .map(|component| component.corrected_source_rotation)
                .collect::<Vec<_>>()
        };
        assert_eq!(corrected(&standard), [None, None]);
        assert_eq!(corrected(&defective), [Some(30.0), None]);
    }

    #[test]
    fn recognizes_the_defective_kicad_releases() {
        for revision in [
            "9.0.0", "9.0.7", "10.0.5", "10.0.6", "11.0.0", "9.99.0", "nightly",
        ] {
            assert_eq!(written_by(revision), None, "{revision}");
        }
        // An orientation of 30° on the bottom is -210° in standard form.
        for (revision, written) in [
            ("9.0.8", -30.0),
            ("9.0.9", -30.0),
            ("10.0.0", 30.0),
            ("10.0.4", 30.0),
        ] {
            let defect = written_by(revision).unwrap();
            assert_eq!(defect.exporter, format!("KiCad {revision}"));
            assert_eq!(defect.standard_rotation(written), -210.0, "{revision}");
        }
    }
}
