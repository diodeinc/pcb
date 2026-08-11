use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use pcb_sexpr::{PatchSet, Sexpr, formatter::FormatMode};

use crate::{SchDocument, SchPage};

pub(crate) fn patch_page_source(source: &str, desired_page: &SchPage) -> Result<Option<String>> {
    let desired_source = SchDocument {
        pages: vec![desired_page.clone()],
        root_page_ids: vec![desired_page.id.clone()],
    }
    .to_kicad_sch()?;
    let source_root = pcb_sexpr::parse(source).context("failed to parse source schematic")?;
    let desired_root =
        pcb_sexpr::parse(&desired_source).context("failed to parse desired schematic")?;
    let source_nodes = managed_nodes(&source_root)?;
    let desired_nodes = managed_nodes(&desired_root)?;
    let mut patches = PatchSet::new();

    for (key, source_node) in &source_nodes {
        match desired_nodes.get(key) {
            Some(desired_node) if *source_node != *desired_node => patches.replace_raw(
                source_node.span,
                pcb_sexpr::formatter::format_tree(desired_node, FormatMode::Normal)
                    .trim()
                    .to_string(),
            ),
            Some(_) => {}
            None => patches.replace_raw(source_node.span, String::new()),
        }
    }

    let additions = desired_nodes
        .iter()
        .filter(|(key, _)| !source_nodes.contains_key(*key))
        .map(|(_, node)| {
            pcb_sexpr::formatter::format_tree(node, FormatMode::Normal)
                .trim()
                .to_string()
        })
        .collect::<Vec<_>>();
    if !additions.is_empty() {
        let insertion = trailing_section_start(&source_root)?.unwrap_or(
            source_root
                .span
                .end
                .checked_sub(1)
                .context("schematic root has an invalid span")?,
        );
        patches.replace_raw(
            pcb_sexpr::Span::new(insertion, insertion),
            format!("\n{}\n", additions.join("\n")),
        );
    }

    if patches.is_empty() {
        return Ok(None);
    }
    let mut patched = Vec::new();
    patches.write_to(source, &mut patched)?;
    String::from_utf8(patched)
        .context("patched schematic is not UTF-8")
        .map(Some)
}

fn trailing_section_start(root: &Sexpr) -> Result<Option<usize>> {
    let items = root.as_list().context("expected kicad_sch root list")?;
    Ok(items.iter().skip(1).find_map(|node| {
        let tag = node
            .as_list()
            .and_then(|items| items.first())
            .and_then(Sexpr::as_sym)?;
        matches!(tag, "sheet_instances" | "embedded_fonts").then_some(node.span.start)
    }))
}

fn managed_nodes(root: &Sexpr) -> Result<BTreeMap<String, &Sexpr>> {
    let items = root.as_list().context("expected kicad_sch root list")?;
    if items.first().and_then(Sexpr::as_sym) != Some("kicad_sch") {
        bail!("expected kicad_sch root");
    }
    let mut result = BTreeMap::new();
    for node in &items[1..] {
        let Some(key) = managed_node_key(node) else {
            continue;
        };
        if result.insert(key.clone(), node).is_some() {
            bail!("schematic contains duplicate source identity '{key}'");
        }
    }
    Ok(result)
}

fn managed_node_key(node: &Sexpr) -> Option<String> {
    let items = node.as_list()?;
    let tag = items.first()?.as_sym()?;
    if tag == "lib_symbols" {
        return Some(tag.to_string());
    }
    if !matches!(
        tag,
        "symbol"
            | "wire"
            | "junction"
            | "no_connect"
            | "label"
            | "global_label"
            | "hierarchical_label"
            | "directive_label"
            | "sheet"
    ) {
        return None;
    }
    let uuid = items.iter().skip(1).find_map(|child| {
        let child = child.as_list()?;
        (child.first()?.as_sym()? == "uuid")
            .then(|| child.get(1)?.as_atom())
            .flatten()
    })?;
    Some(format!("{tag}:{uuid}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Label, LabelKind, LabelShape, Point, SchItem, parse_kicad_sch_page};

    #[test]
    fn patches_managed_items_without_replacing_unsupported_sections() {
        let source = r#"(kicad_sch
  (version 20260306)
  (generator "fixture")
  (uuid "root")
  (paper "A4")
  (lib_symbols)
  (text "preserve me" (at 10 10 0) (effects (font (size 1 1))) (uuid "note")))
  (sheet_instances (path "/" (page "1")))
  (embedded_fonts no)
"#;
        let source = format!("{source})\n");
        let mut page = parse_kicad_sch_page(Some("main.kicad_sch"), &source).unwrap();
        let mut label = Label::new("net-label", "N1", Point::new(20.0, 20.0));
        label.kind = LabelKind::Global {
            shape: LabelShape::Bidirectional,
        };
        page.items.push(SchItem::Label(label));

        let patched = patch_page_source(&source, &page).unwrap().unwrap();
        assert!(patched.contains("preserve me"));
        assert!(patched.contains("global_label \"N1\""));
        assert!(patched.contains(
            "  (text \"preserve me\" (at 10 10 0) (effects (font (size 1 1))) (uuid \"note\"))"
        ));
        assert!(
            patched.find("global_label \"N1\"").unwrap() < patched.find("sheet_instances").unwrap()
        );
    }
}
