use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use pcb_sexpr::{PatchSet, Sexpr, formatter::FormatMode};

use crate::{SchItem, SchPage, SymbolLibrary, parse_kicad_sch_page};

/// Patch one parsed page into its original KiCad source.
///
/// Unchanged semantic items and unsupported source sections retain their
/// original text. The function performs no I/O and returns `None` when the
/// desired page is semantically unchanged. Pages pcb itself wrote are kept
/// canonical instead, so an older pcb layout migrates in one rewrite. An
/// actual edit to a KiCad 9 page rewrites it as KiCad 10, rather than mixing
/// modern nodes with legacy version-dependent string and body-style semantics.
pub fn patch_page_source(source: &str, desired_page: &SchPage) -> Result<Option<String>> {
    let desired_source = desired_page.to_kicad_sch();
    let source_root = pcb_sexpr::parse(source).context("failed to parse source schematic")?;
    if header(&source_root, "generator").and_then(Sexpr::as_atom) == Some(crate::kicad::GENERATOR) {
        return Ok((desired_source != source).then_some(desired_source));
    }
    let desired_root =
        pcb_sexpr::parse(&desired_source).context("failed to parse desired schematic")?;
    let source_nodes = managed_nodes(&source_root)?;
    let desired_nodes = managed_nodes(&desired_root)?;
    let source_page = parse_kicad_sch_page(desired_page.file_name.as_deref(), source)?;
    let source_values = managed_values(&source_page);
    let desired_values = managed_values(desired_page);
    let mut patches = PatchSet::new();

    for (key, source_node) in &source_nodes {
        match (desired_nodes.get(key), desired_values.get(key)) {
            (Some(desired_node), Some(desired_value))
                if source_values.get(key) != Some(desired_value) =>
            {
                patches.replace_raw(source_node.span, format_node(desired_node))
            }
            (Some(_), Some(_)) => {}
            (None, None) => patches.replace_raw(line_span(source, source_node.span), String::new()),
            _ => bail!("managed schematic identity '{key}' has inconsistent semantic data"),
        }
    }

    // New nodes go before the next node the source already has.
    let mut pending = Vec::new();
    let mut insert = |at: usize, nodes: &mut Vec<String>| {
        let text = nodes
            .drain(..)
            .map(|node| format!("\t{node}\n"))
            .collect::<String>();
        patches.replace_raw(pcb_sexpr::Span::new(at, at), text);
    };
    for node in &desired_root
        .as_list()
        .context("expected kicad_sch root list")?[1..]
    {
        let Some(key) = managed_node_key(node) else {
            continue;
        };
        match source_nodes.get(&key) {
            Some(source_node) if !pending.is_empty() => {
                insert(line_start(source, source_node.span.start), &mut pending)
            }
            Some(_) => {}
            None => pending.push(format_node(node)),
        }
    }
    if !pending.is_empty() {
        let end = trailing_section_start(&source_root)?.unwrap_or(
            source_root
                .span
                .end
                .checked_sub(1)
                .context("schematic root has an invalid span")?,
        );
        insert(line_start(source, end), &mut pending);
    }

    if patches.is_empty() {
        return Ok(None);
    }
    if header(&source_root, "version").and_then(Sexpr::as_int) == Some(20250114) {
        return Ok(Some(desired_source));
    }
    let mut patched = Vec::new();
    patches.write_to(source, &mut patched)?;
    String::from_utf8(patched)
        .context("patched schematic is not UTF-8")
        .map(Some)
}

fn header<'a>(root: &'a Sexpr, tag: &str) -> Option<&'a Sexpr> {
    pcb_sexpr::find_child_list(root.as_list()?, tag)?.get(1)
}

fn format_node(node: &Sexpr) -> String {
    pcb_sexpr::formatter::format_tree(node, FormatMode::Normal)
        .trim()
        .replace('\n', "\n\t")
}

/// Start of `offset`'s line, unless another node precedes it on that line.
fn line_start(source: &str, offset: usize) -> usize {
    let start = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    if source[start..offset].trim().is_empty() {
        start
    } else {
        offset
    }
}

fn line_span(source: &str, span: pcb_sexpr::Span) -> pcb_sexpr::Span {
    let start = line_start(source, span.start);
    let end = source[span.end..]
        .find('\n')
        .map_or(source.len(), |index| span.end + index + 1);
    if source[span.end..end].trim().is_empty() {
        pcb_sexpr::Span::new(start, end)
    } else {
        span
    }
}

#[derive(PartialEq)]
enum ManagedValue<'a> {
    Library(&'a SymbolLibrary),
    Item(&'a SchItem),
}

fn managed_values(page: &SchPage) -> BTreeMap<String, ManagedValue<'_>> {
    let mut values = BTreeMap::from([(
        "lib_symbols".to_string(),
        ManagedValue::Library(&page.library),
    )]);
    for item in &page.items {
        if matches!(item, SchItem::Sheet(sheet) if !sheet.placed) {
            continue;
        }
        let Some(id) = item.id() else {
            continue;
        };
        let tag = match item {
            SchItem::Symbol(_) => "symbol",
            SchItem::Wire(_) => "wire",
            SchItem::Junction(_) => "junction",
            SchItem::NoConnect(_) => "no_connect",
            SchItem::Label(label) => crate::kicad::label_kind_token(label.kind),
            SchItem::Sheet(_) => "sheet",
            SchItem::Graphic(graphic) => graphic.kind.kicad_tag(),
            SchItem::Unsupported(_) => continue,
        };
        values.insert(format!("{tag}:{id}"), ManagedValue::Item(item));
    }
    values
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
            | "netclass_flag"
            | "directive_label"
            | "sheet"
            | "rectangle"
            | "polyline"
            | "circle"
            | "arc"
            | "text"
            | "text_box"
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
    fn editing_kicad_9_upgrades_the_page_without_reinterpreting_new_literal_tildes() {
        let source = include_str!("../../pcb-sch/test/kicad-bom/layout.kicad_sch");
        let mut page = parse_kicad_sch_page(None, source).unwrap();
        assert_eq!(patch_page_source(source, &page).unwrap(), None);
        let symbol = page
            .items
            .iter_mut()
            .find_map(|item| {
                if let SchItem::Symbol(symbol) = item {
                    Some(symbol)
                } else {
                    None
                }
            })
            .unwrap();
        symbol.fields.get_mut("Value").unwrap().value = "~".to_owned();
        let saved = patch_page_source(source, &page).unwrap().unwrap();
        assert!(saved.contains("(version 20260306)"));
        assert_eq!(parse_kicad_sch_page(None, &saved).unwrap(), page);
        assert_eq!(patch_page_source(&saved, &page).unwrap(), None);
    }

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
        let mut directive = Label::new("directive", "", Point::new(30.0, 20.0));
        directive.kind = LabelKind::Directive {
            shape: LabelShape::Round,
        };
        page.items.push(SchItem::Label(directive));

        let patched = patch_page_source(&source, &page).unwrap().unwrap();
        assert!(patched.contains("preserve me"));
        assert!(patched.contains("global_label \"N1\""));
        assert!(patched.contains("netclass_flag \"\""));
        assert!(patched.contains(
            "  (text \"preserve me\" (at 10 10 0) (effects (font (size 1 1))) (uuid \"note\"))"
        ));
        assert!(
            patched.find("global_label \"N1\"").unwrap() < patched.find("sheet_instances").unwrap()
        );
    }
}
