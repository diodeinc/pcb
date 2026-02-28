use anyhow::Context;
use base64::Engine;
use pcb_sexpr::{Sexpr, SexprKind, WalkCtx, find_named_list_index, set_or_insert_named_list};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModelEmbedDiscoveryStats {
    pub total_refs: usize,
    pub already_embedded: usize,
    pub managed_refs: usize,
    pub unmanaged_refs: usize,
    pub unresolved_refs: usize,
    pub missing_files: usize,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModelEmbedApplyStats {
    pub candidate_files: usize,
    pub rewritten_refs: usize,
    pub embedded_files_added: usize,
    pub footprint_metadata_entries: usize,
    pub basename_collisions: usize,
}

enum DiscoveryOutcome {
    AlreadyEmbedded,
    ManagedCandidate {
        source_path: PathBuf,
        embed_name: String,
    },
    ManagedMissing,
    Unmanaged,
    Unresolved,
}

pub(crate) fn embed_models_in_pcb_source(
    source: &str,
    pcb_dir: &Path,
    kicad_model_dirs: &BTreeMap<String, PathBuf>,
) -> anyhow::Result<(String, ModelEmbedDiscoveryStats, ModelEmbedApplyStats)> {
    let mut board = pcb_sexpr::parse(source).context("Failed to parse PCB file for model embed")?;
    let (replacements, files_to_embed, discovery_stats, mut apply_stats) =
        collect_embed_plan(&board, pcb_dir, kicad_model_dirs);

    let mut model_checksums = existing_embedded_model_checksums(&board);
    let mut new_file_nodes = Vec::new();
    for (embed_name, source_path) in &files_to_embed {
        if model_checksums.contains_key(embed_name) {
            continue;
        }

        let bytes = std::fs::read(source_path)
            .with_context(|| format!("Failed to read 3D model {}", source_path.display()))?;
        let checksum = sha256_hex(&bytes);
        model_checksums.insert(embed_name.clone(), checksum.clone());
        let data = compress_and_encode(&bytes)?;
        new_file_nodes.push(build_model_file_node(embed_name, &checksum, Some(&data)));
        apply_stats.embedded_files_added += 1;
    }

    if !new_file_nodes.is_empty() {
        let root_items = board
            .as_list_mut()
            .ok_or_else(|| anyhow::anyhow!("KiCad PCB root is not a list"))?;
        let embedded_files = ensure_named_list_mut(root_items, "embedded_files", Some("setup"))
            .ok_or_else(|| {
                anyhow::anyhow!("Failed to create or locate (embedded_files ...) on PCB root")
            })?;
        embedded_files.extend(new_file_nodes);
    }

    rewrite_model_paths(&mut board, &replacements, &mut apply_stats);
    apply_stats.footprint_metadata_entries =
        upsert_footprint_embedded_model_metadata(&mut board, &model_checksums);

    let changed = apply_stats.embedded_files_added > 0
        || apply_stats.rewritten_refs > 0
        || apply_stats.footprint_metadata_entries > 0;

    let output = if changed {
        pcb_sexpr::formatter::format_tree(&board, pcb_sexpr::formatter::FormatMode::Normal)
    } else {
        source.to_string()
    };
    Ok((output, discovery_stats, apply_stats))
}

fn is_model_filename(ctx: &WalkCtx<'_>) -> bool {
    ctx.parent_tag() == Some("model") && ctx.index_in_parent == Some(1)
}

fn resolve_managed_model_reference(
    model_ref: &str,
    pcb_dir: &Path,
    kicad_model_dirs: &BTreeMap<String, PathBuf>,
) -> DiscoveryOutcome {
    if model_ref.starts_with("kicad-embed://") {
        return DiscoveryOutcome::AlreadyEmbedded;
    }

    if let Some((var_name, rest)) = parse_leading_var_reference(model_ref) {
        if var_name == "KIPRJMOD" {
            return DiscoveryOutcome::Unmanaged;
        }

        let Some(var_root) = kicad_model_dirs.get(var_name) else {
            return DiscoveryOutcome::Unresolved;
        };
        return to_candidate_or_missing(var_root.join(strip_leading_separators(rest)));
    }

    if model_ref.starts_with("${") || model_ref.starts_with("$(") {
        return DiscoveryOutcome::Unresolved;
    }

    let path = Path::new(model_ref);
    if path.is_absolute() {
        for root in kicad_model_dirs.values() {
            if path.starts_with(root) {
                return to_candidate_or_missing(path.to_path_buf());
            }
        }
        return DiscoveryOutcome::Unmanaged;
    }

    let resolved = pcb_dir.join(path);
    for root in kicad_model_dirs.values() {
        if resolved.starts_with(root) {
            return to_candidate_or_missing(resolved);
        }
    }

    DiscoveryOutcome::Unmanaged
}

fn to_candidate_or_missing(managed_path: PathBuf) -> DiscoveryOutcome {
    let Some(source_path) = select_embeddable_source_path(&managed_path) else {
        return DiscoveryOutcome::ManagedMissing;
    };
    if !source_path.is_file() {
        return DiscoveryOutcome::ManagedMissing;
    }

    let Some(embed_name) = source_path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
    else {
        return DiscoveryOutcome::ManagedMissing;
    };

    DiscoveryOutcome::ManagedCandidate {
        source_path,
        embed_name,
    }
}

fn select_embeddable_source_path(path: &Path) -> Option<PathBuf> {
    if is_wrl_path(path) {
        return step_sidecar_path(path);
    }
    Some(path.to_path_buf())
}

fn is_wrl_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wrl") || ext.eq_ignore_ascii_case("wrz"))
}

fn step_sidecar_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let stem = path.file_stem()?.to_str()?;
    [format!("{stem}.step"), format!("{stem}.stp")]
        .into_iter()
        .map(|name| parent.join(name))
        .find(|candidate| candidate.is_file())
}

fn parse_leading_var_reference(input: &str) -> Option<(&str, &str)> {
    if let Some(rest) = input.strip_prefix("${") {
        let end = rest.find('}')?;
        let var = &rest[..end];
        let suffix = &rest[end + 1..];
        return Some((var, suffix));
    }

    if let Some(rest) = input.strip_prefix("$(") {
        let end = rest.find(')')?;
        let var = &rest[..end];
        let suffix = &rest[end + 1..];
        return Some((var, suffix));
    }

    None
}

fn strip_leading_separators(path: &str) -> &str {
    path.trim_start_matches(['/', '\\'])
}

fn collect_embed_plan(
    board: &Sexpr,
    pcb_dir: &Path,
    kicad_model_dirs: &BTreeMap<String, PathBuf>,
) -> (
    BTreeMap<String, String>,
    BTreeMap<String, PathBuf>,
    ModelEmbedDiscoveryStats,
    ModelEmbedApplyStats,
) {
    let mut discovery = ModelEmbedDiscoveryStats::default();
    let mut apply = ModelEmbedApplyStats::default();
    let mut replacements = BTreeMap::<String, String>::new();
    let mut files_to_embed = BTreeMap::<String, PathBuf>::new();

    board.walk_strings(|value, _span, ctx| {
        if !is_model_filename(&ctx) {
            return;
        }

        discovery.total_refs += 1;
        match resolve_managed_model_reference(value, pcb_dir, kicad_model_dirs) {
            DiscoveryOutcome::AlreadyEmbedded => discovery.already_embedded += 1,
            DiscoveryOutcome::ManagedCandidate {
                source_path,
                embed_name,
            } => {
                discovery.managed_refs += 1;
                // Accepted risk: basename collisions are possible (KiCad-style behavior).
                // For now we keep the first file for a basename and skip conflicting ones.
                match files_to_embed.get(&embed_name) {
                    Some(existing) if existing != &source_path => {
                        apply.basename_collisions += 1;
                    }
                    Some(_) => {}
                    None => {
                        files_to_embed.insert(embed_name.clone(), source_path);
                    }
                }
                replacements.insert(value.to_string(), format!("kicad-embed://{embed_name}"));
            }
            DiscoveryOutcome::ManagedMissing => {
                discovery.managed_refs += 1;
                discovery.missing_files += 1;
            }
            DiscoveryOutcome::Unmanaged => discovery.unmanaged_refs += 1,
            DiscoveryOutcome::Unresolved => discovery.unresolved_refs += 1,
        }
    });

    apply.candidate_files = files_to_embed.len();

    (replacements, files_to_embed, discovery, apply)
}

fn existing_embedded_model_checksums(board: &Sexpr) -> BTreeMap<String, String> {
    let mut checksums = BTreeMap::new();
    let Some(root_items) = board.as_list() else {
        return checksums;
    };
    let Some(embedded_idx) = find_named_list_index(root_items, "embedded_files") else {
        return checksums;
    };
    let Some(embedded_items) = root_items[embedded_idx].as_list() else {
        return checksums;
    };

    for file in embedded_items.iter().skip(1) {
        let Some(file_items) = file.as_list() else {
            continue;
        };
        if list_tag(file_items) != Some("file") {
            continue;
        }
        if child_sym(file_items, "type") == Some("model")
            && let (Some(name), Some(checksum)) = (
                child_sym(file_items, "name"),
                child_str(file_items, "checksum"),
            )
        {
            checksums.insert(name.to_string(), checksum.to_string());
        }
    }

    checksums
}

fn build_model_file_node(name: &str, checksum: &str, data: Option<&str>) -> Sexpr {
    let mut items = vec![
        Sexpr::symbol("file"),
        Sexpr::list(vec![Sexpr::symbol("name"), Sexpr::symbol(name.to_string())]),
        Sexpr::list(vec![Sexpr::symbol("type"), Sexpr::symbol("model")]),
    ];
    if let Some(data) = data {
        items.push(Sexpr::list(vec![
            Sexpr::symbol("data"),
            Sexpr::symbol(format!("|{data}|")),
        ]));
    }
    items.push(Sexpr::list(vec![
        Sexpr::symbol("checksum"),
        Sexpr::string(checksum.to_string()),
    ]));
    Sexpr::list(items)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn compress_and_encode(bytes: &[u8]) -> anyhow::Result<String> {
    let mut encoder = zstd::Encoder::new(Vec::new(), 17)?;
    encoder.include_contentsize(true)?;
    encoder.set_pledged_src_size(Some(bytes.len() as u64))?;
    encoder.write_all(bytes)?;
    let compressed = encoder.finish()?;
    Ok(base64::engine::general_purpose::STANDARD.encode(compressed))
}

fn rewrite_model_paths(
    node: &mut Sexpr,
    replacements: &BTreeMap<String, String>,
    apply: &mut ModelEmbedApplyStats,
) {
    let Some(items) = node.as_list_mut() else {
        return;
    };

    if let Some(new_value) = model_path_replacement(items, replacements) {
        let SexprKind::String(path) = &mut items[1].kind else {
            unreachable!();
        };
        *path = new_value;
        apply.rewritten_refs += 1;
    }

    for child in items.iter_mut() {
        rewrite_model_paths(child, replacements, apply);
    }
}

fn model_path_replacement(
    items: &[Sexpr],
    replacements: &BTreeMap<String, String>,
) -> Option<String> {
    if list_tag(items) != Some("model") {
        return None;
    }
    let SexprKind::String(path) = &items.get(1)?.kind else {
        return None;
    };
    let new_value = replacements.get(path)?;
    (path != new_value).then(|| new_value.clone())
}

fn upsert_footprint_embedded_model_metadata(
    node: &mut Sexpr,
    checksums: &BTreeMap<String, String>,
) -> usize {
    let Some(items) = node.as_list_mut() else {
        return 0;
    };

    let mut changed = 0;
    if list_tag(items) == Some("footprint") {
        changed += upsert_one_footprint_metadata(items, checksums);
    }

    for child in items.iter_mut() {
        changed += upsert_footprint_embedded_model_metadata(child, checksums);
    }
    changed
}

fn upsert_one_footprint_metadata(
    footprint_items: &mut Vec<Sexpr>,
    checksums: &BTreeMap<String, String>,
) -> usize {
    let mut embed_names = BTreeSet::new();
    collect_embed_model_names_in_items(footprint_items, &mut embed_names);
    if embed_names.is_empty() {
        return 0;
    }

    let Some(embedded_items) = ensure_named_list_mut(footprint_items, "embedded_files", None)
    else {
        return 0;
    };

    let file_index: BTreeMap<String, usize> = embedded_items
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(idx, file)| {
            let items = file.as_list()?;
            if list_tag(items) != Some("file") {
                return None;
            }
            Some((child_sym(items, "name")?.to_string(), idx))
        })
        .collect();

    let mut changed = 0;
    for name in embed_names {
        let Some(checksum) = checksums.get(&name) else {
            log::warn!(
                "Missing checksum for embedded model '{}' while patching footprint metadata",
                name
            );
            continue;
        };
        let new_node = build_model_file_node(&name, checksum, None);
        match file_index.get(&name).copied() {
            Some(idx) if embedded_items[idx] != new_node => {
                embedded_items[idx] = new_node;
                changed += 1;
            }
            None => {
                embedded_items.push(new_node);
                changed += 1;
            }
            _ => {}
        }
    }

    changed
}

fn list_tag(items: &[Sexpr]) -> Option<&str> {
    items.first().and_then(Sexpr::as_sym)
}

fn child_sym<'a>(parent: &'a [Sexpr], tag: &str) -> Option<&'a str> {
    parent.iter().skip(1).find_map(|child| {
        let child_items = child.as_list()?;
        (list_tag(child_items) == Some(tag))
            .then(|| child_items.get(1).and_then(Sexpr::as_sym))
            .flatten()
    })
}

fn child_str<'a>(parent: &'a [Sexpr], tag: &str) -> Option<&'a str> {
    parent.iter().skip(1).find_map(|child| {
        let child_items = child.as_list()?;
        (list_tag(child_items) == Some(tag))
            .then(|| child_items.get(1).and_then(Sexpr::as_str))
            .flatten()
    })
}

fn ensure_named_list_mut<'a>(
    items: &'a mut Vec<Sexpr>,
    name: &str,
    before_name: Option<&str>,
) -> Option<&'a mut Vec<Sexpr>> {
    if find_named_list_index(items, name).is_none() {
        let list = Sexpr::list(vec![Sexpr::symbol(name)]);
        set_or_insert_named_list(items, name, list, before_name);
    }
    let idx = find_named_list_index(items, name)?;
    items.get_mut(idx)?.as_list_mut()
}

fn embedded_model_name(items: &[Sexpr]) -> Option<&str> {
    if list_tag(items) != Some("model") {
        return None;
    }
    let SexprKind::String(path) = &items.get(1)?.kind else {
        return None;
    };
    let name = path.strip_prefix("kicad-embed://")?;
    (!name.is_empty()).then_some(name)
}

fn collect_embed_model_names_in_items(items: &[Sexpr], out: &mut BTreeSet<String>) {
    if let Some(name) = embedded_model_name(items) {
        out.insert(name.to_string());
    }
    for child in items.iter().skip(1) {
        if let Some(child_items) = child.as_list() {
            collect_embed_model_names_in_items(child_items, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn embeds_existing_managed_models() {
        let temp = tempdir().unwrap();
        let model_root = temp.path().join("cache/models/9.0.3");
        let model_rel = Path::new("Resistor_SMD.3dshapes/R_0603_1608Metric.step");
        let model_path = model_root.join(model_rel);
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"step").unwrap();

        let source = r#"(kicad_pcb
  (footprint "R"
    (model "${KICAD9_3DMODEL_DIR}/Resistor_SMD.3dshapes/R_0603_1608Metric.step")
  )
)"#;

        let dirs = BTreeMap::from([("KICAD9_3DMODEL_DIR".to_string(), model_root.clone())]);
        let (embedded, stats, apply) =
            embed_models_in_pcb_source(source, temp.path(), &dirs).unwrap();

        assert_eq!(stats.total_refs, 1);
        assert_eq!(stats.managed_refs, 1);
        assert_eq!(stats.missing_files, 0);
        assert_eq!(apply.candidate_files, 1);
        assert_eq!(apply.rewritten_refs, 1);
        assert_eq!(apply.embedded_files_added, 1);
        assert_eq!(apply.footprint_metadata_entries, 1);
        assert!(embedded.contains("kicad-embed://R_0603_1608Metric.step"));
        assert_eq!(embedded.matches("(name R_0603_1608Metric.step)").count(), 2);
        assert_eq!(embedded.matches("(data |").count(), 1);
    }

    #[test]
    fn ignores_embedded_and_unmanaged_and_reports_unresolved() {
        let temp = tempdir().unwrap();
        let source = r#"(kicad_pcb
  (footprint "A" (model "kicad-embed://already.step"))
  (footprint "B" (model "${KIPRJMOD}/local/model.step"))
  (footprint "C" (model "${UNKNOWN_DIR}/part.step"))
)"#;

        let (embedded, stats, apply) =
            embed_models_in_pcb_source(source, temp.path(), &BTreeMap::new()).unwrap();

        assert_eq!(embedded, source);
        assert_eq!(stats.total_refs, 3);
        assert_eq!(stats.already_embedded, 1);
        assert_eq!(stats.unmanaged_refs, 1);
        assert_eq!(stats.unresolved_refs, 1);
        assert_eq!(apply.candidate_files, 0);
        assert_eq!(apply.footprint_metadata_entries, 0);
    }

    #[test]
    fn adds_footprint_metadata_for_existing_embedded_reference() {
        let temp = tempdir().unwrap();
        let source = r#"(kicad_pcb
  (embedded_files
    (file
      (name Foo.step)
      (type model)
      (data |AAAA|)
      (checksum "deadbeef")
    )
  )
  (footprint "U"
    (model "kicad-embed://Foo.step")
  )
)"#;

        let (embedded, stats, apply) =
            embed_models_in_pcb_source(source, temp.path(), &BTreeMap::new()).unwrap();

        assert_eq!(stats.total_refs, 1);
        assert_eq!(stats.already_embedded, 1);
        assert_eq!(apply.rewritten_refs, 0);
        assert_eq!(apply.embedded_files_added, 0);
        assert_eq!(apply.footprint_metadata_entries, 1);
        assert_eq!(embedded.matches("(name Foo.step)").count(), 2);
        assert_eq!(embedded.matches("(data |AAAA|)").count(), 1);
    }

    #[test]
    fn rewrites_wrl_reference_to_embedded_step() {
        let temp = tempdir().unwrap();
        let model_root = temp.path().join("cache/models/9.0.3");
        let wrl_path = model_root.join("Pkg3D/Foo.wrl");
        let step_path = model_root.join("Pkg3D/Foo.step");
        std::fs::create_dir_all(step_path.parent().unwrap()).unwrap();
        std::fs::write(&wrl_path, b"wrl").unwrap();
        std::fs::write(&step_path, b"step").unwrap();

        let source = r#"(kicad_pcb
  (footprint "U"
    (model "${KICAD9_3DMODEL_DIR}/Pkg3D/Foo.wrl")
  )
)"#;
        let dirs = BTreeMap::from([("KICAD9_3DMODEL_DIR".to_string(), model_root)]);
        let (embedded, discovery, apply) =
            embed_models_in_pcb_source(source, temp.path(), &dirs).unwrap();

        assert_eq!(discovery.total_refs, 1);
        assert_eq!(discovery.managed_refs, 1);
        assert_eq!(apply.candidate_files, 1);
        assert_eq!(apply.rewritten_refs, 1);
        assert_eq!(apply.embedded_files_added, 1);
        assert_eq!(apply.footprint_metadata_entries, 1);
        assert!(embedded.contains("kicad-embed://Foo.step"));
        assert_eq!(embedded.matches("(name Foo.step)").count(), 2);
        assert!(embedded.contains("(embedded_files"));
        pcb_sexpr::parse(&embedded).expect("embedded board should parse");
    }
}
