use std::collections::BTreeSet;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use ignore::{DirEntry, WalkBuilder};
use pcb_sch::PACKAGE_URI_PREFIX;
use pcb_zen::ast_utils::visit_string_literals;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::config::PcbToml;
use pcb_zen_core::package_url::canonicalize_package_reference;
use pcb_zen_core::workspace::{WORKSPACE_DISCOVERY_EXCLUDE_DIRS, get_workspace_info};
use starlark::codemap::Span;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::{ExprP, StmtP};
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;
use toml_edit::{DocumentMut, Item, Key, TableLike, Value};

#[derive(Default)]
pub(super) struct RegistryMigration {
    pub(super) manifests: usize,
    pub(super) sources: usize,
}

pub(super) fn migrate_registry_references(root: &Path) -> Result<RegistryMigration> {
    let workspace = get_workspace_info(&DefaultFileProvider::new(), root)?;
    if !workspace.errors.is_empty() {
        for error in &workspace.errors {
            eprintln!("{}: {}", error.path.display(), error.error);
        }
        bail!("Found {} invalid pcb.toml file(s)", workspace.errors.len());
    }

    let mut manifest_paths = BTreeSet::from([workspace.root.join("pcb.toml")]);
    let mut package_roots = BTreeSet::new();
    for package in workspace.packages.values() {
        let package_root = package.dir(&workspace.root);
        manifest_paths.insert(package_root.join("pcb.toml"));
        package_roots.insert(package_root);
    }

    let mut edits = Vec::new();
    let mut migration = RegistryMigration::default();
    for path in manifest_paths {
        let source = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if let Some(content) = migrate_manifest(&source, &path)? {
            migration.manifests += 1;
            edits.push((path, content));
        }
    }

    for path in zen_files(package_roots)? {
        let source = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if let Some(content) = migrate_zen_source(&source, &path)? {
            migration.sources += 1;
            edits.push((path, content));
        }
    }

    for (path, content) in edits {
        fs::write(&path, content).with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(migration)
}

fn migrate_manifest(source: &str, path: &Path) -> Result<Option<String>> {
    let mut config = PcbToml::parse_with_path(source, path)?;
    config
        .canonicalize_package_references()
        .with_context(|| format!("Failed to migrate {}", path.display()))?;

    let mut document = source
        .parse::<DocumentMut>()
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    if let Some(dependencies) = document
        .get_mut("dependencies")
        .and_then(Item::as_table_like_mut)
    {
        rewrite_table_keys(dependencies);
        if let Some(indirect) = dependencies
            .get_mut("indirect")
            .and_then(Item::as_table_like_mut)
        {
            rewrite_table_keys(indirect);
        }
    }
    if let Some(patch) = document.get_mut("patch").and_then(Item::as_table_like_mut) {
        rewrite_table_keys(patch);
    }
    if let Some(workspace) = document
        .get_mut("workspace")
        .and_then(Item::as_table_like_mut)
    {
        if let Some(repository) = workspace.get_mut("repository").and_then(Item::as_value_mut) {
            rewrite_value(repository);
        }
        if let Some(vendor) = workspace
            .get_mut("vendor")
            .and_then(Item::as_value_mut)
            .and_then(Value::as_array_mut)
        {
            for pattern in vendor.iter_mut() {
                rewrite_value(pattern);
            }
        }
    }

    let rendered = document.to_string();
    if rendered == source {
        return Ok(None);
    }
    Ok(Some(rendered))
}

fn rewrite_table_keys(table: &mut dyn TableLike) {
    let keys = table
        .iter()
        .map(|(key, _)| key.to_owned())
        .collect::<Vec<_>>();
    for key in keys {
        let canonical = canonicalize_package_reference(&key);
        if canonical == key {
            continue;
        }

        if table.contains_key(canonical.as_ref()) {
            table.remove(&key);
        } else {
            let parsed_key = table.key(&key).expect("key came from the same table");
            let new_key = Key::new(canonical.into_owned())
                .with_leaf_decor(parsed_key.leaf_decor().clone())
                .with_dotted_decor(parsed_key.dotted_decor().clone());
            let item = table.remove(&key).expect("key came from the same table");
            table.entry_format(&new_key).or_insert(item);
        }
    }
}

fn rewrite_value(value: &mut Value) {
    let Some(raw) = value.as_str() else {
        return;
    };
    let canonical = canonicalize_package_reference(raw);
    if canonical == raw {
        return;
    }
    let decor = value.decor().clone();
    *value = Value::from(canonical.into_owned());
    *value.decor_mut() = decor;
}

fn zen_files(package_roots: BTreeSet<PathBuf>) -> Result<BTreeSet<PathBuf>> {
    let mut paths = BTreeSet::new();
    for package_root in package_roots {
        let mut builder = WalkBuilder::new(package_root);
        builder
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(include_migration_entry);
        for entry in builder.build() {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().is_some_and(|extension| extension == "zen") {
                paths.insert(path.to_path_buf());
            }
        }
    }
    Ok(paths)
}

fn include_migration_entry(entry: &DirEntry) -> bool {
    !entry.file_type().is_some_and(|kind| kind.is_dir())
        || !entry
            .file_name()
            .to_str()
            .is_some_and(|name| WORKSPACE_DISCOVERY_EXCLUDE_DIRS.contains(&name))
}

fn migrate_zen_source(source: &str, path: &Path) -> Result<Option<String>> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;
    let filename = path.display().to_string();
    let ast = AstModule::parse(&filename, source.to_owned(), &dialect)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .with_context(|| format!("Failed to parse {}", path.display()))?;

    let mut edits = Vec::new();
    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |value, literal| {
            if !matches!(literal.node, ExprP::Literal(_)) || !is_import_literal(value) {
                return;
            }
            if let Some(edit) = source_edit(source, literal.span, value) {
                edits.push(edit);
            }
        });
    });
    for statement in top_level_stmts(ast.statement()) {
        if let StmtP::Load(load) = &statement.node
            && let Some(edit) = source_edit(source, load.module.span, &load.module.node)
        {
            edits.push(edit);
        }
    }

    Ok(apply_source_edits(source, edits))
}

fn is_import_literal(value: &str) -> bool {
    if value.starts_with(PACKAGE_URI_PREFIX) {
        return true;
    }
    let Some(file_name) = value.rsplit('/').next() else {
        return false;
    };
    [
        ".zen",
        ".kicad_sym",
        ".kicad_mod",
        ".pretty",
        ".step",
        ".wrl",
    ]
    .iter()
    .any(|suffix| file_name.ends_with(suffix))
}

fn canonicalize_source_reference(value: &str) -> Option<String> {
    if let Some(reference) = value.strip_prefix(PACKAGE_URI_PREFIX) {
        let canonical = canonicalize_package_reference(reference);
        return (canonical != reference).then(|| format!("{PACKAGE_URI_PREFIX}{canonical}"));
    }
    let canonical = canonicalize_package_reference(value);
    (canonical != value).then(|| canonical.into_owned())
}

fn source_edit(source: &str, span: Span, value: &str) -> Option<(Range<usize>, String)> {
    let canonical = canonicalize_source_reference(value)?;
    let range = span.begin().get() as usize..span.end().get() as usize;
    let literal = source.get(range.clone())?;
    Some((range, literal.replacen(value, &canonical, 1)))
}

fn apply_source_edits(source: &str, mut edits: Vec<(Range<usize>, String)>) -> Option<String> {
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|(range, _)| range.start);
    let mut rendered = source.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        rendered.replace_range(range, &replacement);
    }
    Some(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = "github.com/diodeinc/registry";
    const CANONICAL: &str = "code.diode.computer/diode/registry";

    #[test]
    fn migrates_manifest_preserving_surrounding_formatting() {
        let source = format!(
            r#"# keep this comment
[workspace]
pcb-version = "0.4"
repository = '{LEGACY}' # repository
vendor = ["{LEGACY}/**", "github.com/example/**"]

[dependencies]
"{LEGACY}/components/Foo" = "1.0" # direct

[dependencies.indirect]
"{LEGACY}/components/Bar@1" = "1.2.0"

[patch]
"{LEGACY}/components/Baz" = {{ path = "../Baz" }}
"#
        );

        let migrated = migrate_manifest(&source, Path::new("pcb.toml"))
            .unwrap()
            .unwrap();

        assert!(!migrated.contains(LEGACY));
        assert!(migrated.contains("# keep this comment"));
        assert!(migrated.contains(&format!("repository = \"{CANONICAL}\" # repository")));
        assert!(migrated.contains(&format!(
            "\"{CANONICAL}/components/Foo\" = \"1.0\" # direct"
        )));
        assert!(
            migrate_manifest(&migrated, Path::new("pcb.toml"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn deduplicates_equal_manifest_keys_and_rejects_conflicts() {
        let equal = format!(
            "[dependencies]\n\
             \"{LEGACY}/components/Foo\" = \"1.0\"\n\
             \"{CANONICAL}/components/Foo\" = \"1.0\"\n"
        );
        let migrated = migrate_manifest(&equal, Path::new("pcb.toml"))
            .unwrap()
            .unwrap();
        assert!(!migrated.contains(LEGACY));
        assert_eq!(migrated.matches("components/Foo").count(), 1);

        let conflict = format!(
            "[dependencies]\n\
             \"{LEGACY}/components/Foo\" = \"1.0\"\n\
             \"{CANONICAL}/components/Foo\" = \"2.0\"\n"
        );
        let error = migrate_manifest(&conflict, Path::new("pcb.toml"))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Failed to migrate pcb.toml"));
    }

    #[test]
    fn migrates_static_source_imports_without_reformatting() {
        let source = format!(
            r#"# {LEGACY}/components/Comment/Comment.zen
load('{LEGACY}/components/Foo/Foo.zen', "Foo")
Bar = Module("{LEGACY}/components/Bar/Bar.zen")
model = "package://{LEGACY}/components/Bar/model.step"
lookalike = "{LEGACY}-old/components/Baz/Baz.zen"
prose = "{LEGACY} is the old registry"
"#
        );

        let migrated = migrate_zen_source(&source, Path::new("board.zen"))
            .unwrap()
            .unwrap();

        assert!(migrated.contains(&format!(
            "load('{CANONICAL}/components/Foo/Foo.zen', \"Foo\")"
        )));
        assert!(migrated.contains(&format!(
            "Bar = Module(\"{CANONICAL}/components/Bar/Bar.zen\")"
        )));
        assert!(migrated.contains(&format!(
            "model = \"package://{CANONICAL}/components/Bar/model.step\""
        )));
        assert!(migrated.contains(&format!("# {LEGACY}/components/Comment/Comment.zen")));
        assert!(migrated.contains(&format!(
            "lookalike = \"{LEGACY}-old/components/Baz/Baz.zen\""
        )));
        assert!(migrated.contains(&format!("prose = \"{LEGACY} is the old registry\"")));
        assert!(
            migrate_zen_source(&migrated, Path::new("board.zen"))
                .unwrap()
                .is_none()
        );
    }
}
