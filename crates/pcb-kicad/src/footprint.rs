use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use base64::Engine;
use pcb_sexpr::formatter::{FormatMode, prettify};
use pcb_sexpr::{PatchSet, Sexpr, Span};
use std::fs;
use std::io::Write;
use std::path::Path;

const EMBED_URI: &str = "kicad-embed://";
const IDENTITY_PLACEMENT: &str = "(offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))";

fn children<'a>(items: &'a [Sexpr], name: &'a str) -> impl Iterator<Item = &'a Sexpr> {
    items.iter().filter(move |node| {
        node.as_list()
            .and_then(<[Sexpr]>::first)
            .and_then(Sexpr::as_sym)
            == Some(name)
    })
}

fn is_shown(model: &Sexpr) -> bool {
    let items = model.as_list().unwrap_or_default();
    !items.iter().any(|item| item.as_sym() == Some("hide"))
        && children(items, "hide")
            .all(|hide| hide.as_list().and_then(|hide| hide.get(1)?.as_sym()) == Some("no"))
}

pub fn format_kicad_sexpr_source(source: &str, path_for_error: &Path) -> Result<String> {
    pcb_sexpr::parse(source)
        .map_err(|e| anyhow::anyhow!(e))
        .with_context(|| {
            format!(
                "Failed to parse KiCad S-expression file {}",
                path_for_error.display()
            )
        })?;

    Ok(prettify(source, FormatMode::Normal))
}

/// Make `step_bytes` the only 3D model of a footprint, embedded in it.
///
/// Every model reference and embedded model already in the footprint is
/// replaced; the new reference keeps the placement of the first one shown.
pub fn embed_step_in_footprint(
    footprint: &str,
    step_bytes: &[u8],
    step_filename: &str,
) -> Result<String> {
    let filename = step_filename.replace(".stp", ".step");

    let mut encoder = zstd::Encoder::new(Vec::new(), 17)?;
    encoder.include_contentsize(true)?;
    encoder.set_pledged_src_size(Some(step_bytes.len() as u64))?;
    encoder.write_all(step_bytes)?;
    let compressed = encoder.finish()?;
    let data = base64::engine::general_purpose::STANDARD
        .encode(&compressed)
        .as_bytes()
        .chunks(80)
        .map(|line| std::str::from_utf8(line).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    let checksum = pcb_sexpr::kicad::footprint::embedded_file_checksum(step_bytes);

    let root = pcb_sexpr::parse(footprint).map_err(|e| anyhow::anyhow!(e))?;
    let items = root.as_list().context("Footprint is not a list")?;

    let text = |node: &Sexpr| &footprint[node.span.start..node.span.end];

    let models = children(items, "model").collect::<Vec<_>>();
    let placement = models
        .iter()
        .find(|model| is_shown(model))
        .or(models.first())
        .map_or_else(
            || IDENTITY_PLACEMENT.to_string(),
            |model| {
                let model = model.as_list().unwrap_or_default();
                ["offset", "at", "scale", "rotate"]
                    .into_iter()
                    .flat_map(|name| children(model, name))
                    .map(text)
                    .collect()
            },
        );
    // Files that only served the replaced models go with them.
    let replaced = models
        .iter()
        .filter_map(|model| model.as_list()?.get(1)?.as_atom()?.strip_prefix(EMBED_URI))
        .chain([filename.as_str()])
        .collect::<Vec<_>>();
    let other_files = children(items, "embedded_files")
        .flat_map(|files| children(files.as_list().unwrap_or_default(), "file"))
        .filter(|file| {
            let field = |name| file.find_list(name)?.get(1)?.as_atom();
            field("type") != Some("model") && field("name").is_none_or(|n| !replaced.contains(&n))
        })
        .map(text)
        .collect::<String>();

    let mut patches = PatchSet::new();
    models
        .into_iter()
        .chain(children(items, "embedded_files"))
        .for_each(|node| patches.replace_raw(node.span, String::new()));
    let end = root.span.end - 1;
    patches.replace_raw(
        Span::new(end, end),
        format!(
            "(embedded_files {other_files}\
             (file (name {filename}) (type model) (data |{data}|) (checksum \"{checksum}\")))\
             (model \"{EMBED_URI}{filename}\" {placement})"
        ),
    );

    let mut embedded = Vec::new();
    patches.write_to(footprint, &mut embedded)?;
    Ok(prettify(
        std::str::from_utf8(&embedded)?,
        FormatMode::Normal,
    ))
}

pub fn embed_step_into_footprint_file(
    footprint_path: &Path,
    step_path: &Path,
    delete_step: bool,
) -> Result<()> {
    let footprint = fs::read_to_string(footprint_path).context("Failed to read footprint file")?;
    let step_bytes = fs::read(step_path).context("Failed to read STEP file")?;
    let step_filename = step_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("model.step");

    let embedded = embed_step_in_footprint(&footprint, &step_bytes, step_filename)
        .with_context(|| format!("Failed to embed into {}", footprint_path.display()))?;

    AtomicFile::new(footprint_path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(embedded.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write footprint file: {err}"))?;

    if delete_step {
        fs::remove_file(step_path).context("Failed to remove standalone STEP file")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pcb_sexpr::kicad::footprint::{embedded_file_checksum, validate_footprint_source};

    #[test]
    fn embed_step_replaces_every_model_and_its_payload() {
        let font_checksum = embedded_file_checksum(b"");
        let footprint = format!(
            r#"(footprint "Test" (layer "F.Cu")
  (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu")) (model "kicad-embed://old.step"
    (hide yes) (offset (xyz 1 2 3)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 90)))
  (embedded_files
    (file (name "a.ttf") (type font) (data |KLUv/SAAAQAA|) (checksum "{font_checksum}"))
    (file (name old.step) (type other) (data |OLD|) (checksum "OLD"))
    (file (name stale.step) (type model) (data |OLD|) (checksum "OLD")))
  (model "/tmp/other.step" (offset (xyz 7 8 9)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))))
"#
        );

        let result = embed_step_in_footprint(&footprint, b"NEW", "new.stp").unwrap();

        assert_eq!(result.matches("(model ").count(), 1);
        assert!(result.contains("(model \"kicad-embed://new.step\""));
        assert!(result.contains("(xyz 7 8 9)") && !result.contains("(xyz 1 2 3)"));
        assert_eq!(result.matches("(file").count(), 2);
        assert!(result.contains("(name \"a.ttf\")") && result.contains("(name new.step)"));
        assert!(!result.contains("old.step") && !result.contains("stale.step"));
        assert!(!result.contains("other.step") && !result.contains("hide"));
        validate_footprint_source(&result).unwrap();

        let again = embed_step_in_footprint(&result, b"NEW", "new.step").unwrap();
        assert_eq!(again, result);
    }
}
