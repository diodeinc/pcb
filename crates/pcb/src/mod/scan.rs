use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use pcb_zen::ast_utils::{skip_vendor, visit_string_literals};
use pcb_zen::cache_index::CacheIndex;
use pcb_zen_core::DefaultFileProvider;
use pcb_zen_core::FileProvider;
use pcb_zen_core::config::{DependencySpec, PcbToml};
use pcb_zen_core::kicad_library::kicad_dependency_aliases;
use pcb_zen_core::load_spec::LoadSpec;
use pcb_zen_core::workspace::package_url_covers;
use starlark::syntax::{AstModule, Dialect};
use starlark_syntax::syntax::ast::StmtP;
use starlark_syntax::syntax::top_level_stmts::top_level_stmts;

#[derive(Debug, Default, Clone)]
pub(crate) struct ScannedDirectDeps {
    pub(crate) remote: BTreeMap<String, DependencySpec>,
    pub(crate) workspace: BTreeSet<String>,
    pub(crate) implicit_remote: BTreeMap<String, DependencySpec>,
}

#[derive(Debug, Default)]
struct CollectedImports {
    aliases: BTreeSet<String>,
    urls: BTreeSet<String>,
    relative_paths: Vec<PathBuf>,
}

pub(crate) fn scan_package_direct_deps(
    workspace_info: &pcb_zen::WorkspaceInfo,
    package_url: &str,
    package_dir: &Path,
    current_config: &PcbToml,
    index: &CacheIndex,
    offline: bool,
) -> Result<ScannedDirectDeps> {
    let kicad_entries = workspace_info.kicad_library_entries();
    let kicad_aliases = kicad_dependency_aliases(&kicad_entries);
    let configured_kicad_versions = workspace_info.stdlib_asset_dep_versions();
    let file_provider = DefaultFileProvider::new();
    let package_index = WorkspacePackageIndex::new(workspace_info);
    let mut scanned = ScannedDirectDeps::default();

    if let Some(version) = configured_kicad_versions.get(KICAD_SYMBOLS_REPO) {
        scanned.implicit_remote.insert(
            KICAD_SYMBOLS_REPO.to_string(),
            DependencySpec::Version(version.to_string()),
        );
    }

    for zen_path in package_index.zen_files(package_dir, package_url) {
        let content = file_provider
            .read_file(&zen_path)
            .with_context(|| format!("Failed to read {}", zen_path.display()))?;
        let extracted = extract_imports(&content)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse {}", zen_path.display()))?;

        for alias in extracted.aliases {
            if let Some(repo_url) = kicad_aliases.get(&alias) {
                add_remote_dep(
                    &mut scanned.remote,
                    repo_url,
                    current_config,
                    &configured_kicad_versions,
                    index,
                    offline,
                )?;
            }
        }

        for url in extracted.urls {
            if let Some(member_url) = workspace_member_for_url(workspace_info, &url) {
                if member_url == package_url {
                    anyhow::bail!(
                        "{} uses package URL '{}' that points into its own package '{}'; use a relative path instead",
                        zen_path.display(),
                        url,
                        package_url
                    );
                }
                scanned.workspace.insert(member_url.to_string());
                continue;
            }

            add_remote_dep(
                &mut scanned.remote,
                &url,
                current_config,
                &configured_kicad_versions,
                index,
                offline,
            )?;
        }

        for rel_path in extracted.relative_paths {
            let file_dir = zen_path.parent().unwrap_or(package_dir);
            let Ok(resolved) = file_dir.join(&rel_path).canonicalize() else {
                continue;
            };
            if let Some(member_url) = package_index.owner_for_path(&resolved)
                && member_url != package_url
            {
                scanned.workspace.insert(member_url.to_string());
            }
        }
    }

    Ok(scanned)
}

const KICAD_SYMBOLS_REPO: &str = "gitlab.com/kicad/libraries/kicad-symbols";

struct WorkspacePackageIndex {
    package_dirs: BTreeMap<PathBuf, String>,
}

impl WorkspacePackageIndex {
    fn new(workspace_info: &pcb_zen::WorkspaceInfo) -> Self {
        let package_dirs = workspace_info
            .packages
            .iter()
            .map(|(url, pkg)| {
                let dir = pkg.dir(&workspace_info.root);
                let dir = dir.canonicalize().unwrap_or(dir);
                (dir, url.clone())
            })
            .collect();
        Self { package_dirs }
    }

    fn owner_for_path(&self, path: &Path) -> Option<&str> {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.package_dirs
            .range(..=path.clone())
            .rev()
            .find(|(package_dir, _)| path.starts_with(package_dir))
            .map(|(_, url)| url.as_str())
    }

    fn zen_files(&self, package_dir: &Path, package_url: &str) -> Vec<PathBuf> {
        let mut builder = WalkBuilder::new(package_dir);
        builder
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .filter_entry(skip_vendor);

        let mut files: Vec<_> = builder
            .build()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.into_path())
            .filter(|path| {
                path.is_file()
                    && path.extension() == Some(std::ffi::OsStr::new("zen"))
                    && self.owner_for_path(path) == Some(package_url)
            })
            .collect();

        files.sort();
        files
    }
}

fn add_remote_dep(
    remote: &mut BTreeMap<String, DependencySpec>,
    url: &str,
    current_config: &PcbToml,
    configured_kicad_versions: &BTreeMap<String, semver::Version>,
    index: &CacheIndex,
    offline: bool,
) -> Result<()> {
    if let Some((module_path, spec)) = existing_manifest_dep(url, current_config) {
        remote.entry(module_path).or_insert(spec);
        return Ok(());
    }

    if let Some((module_path, spec)) = resolve_kicad_url(url, configured_kicad_versions) {
        remote.entry(module_path).or_insert(spec);
        return Ok(());
    }

    if offline {
        anyhow::bail!(
            "Cannot discover package root for '{}' in offline mode; add it to [dependencies] first",
            url
        );
    }

    let Some(candidate) = index.find_remote_package(url)? else {
        anyhow::bail!("No remote package found covering '{}'", url);
    };

    remote
        .entry(candidate.module_path)
        .or_insert(DependencySpec::Version(candidate.version));
    Ok(())
}

fn existing_manifest_dep(url: &str, config: &PcbToml) -> Option<(String, DependencySpec)> {
    config
        .dependencies
        .iter()
        .filter(|(module_path, _)| package_url_covers(module_path, url))
        .max_by_key(|(module_path, _)| module_path.len())
        .map(|(module_path, spec)| (module_path.clone(), spec.clone()))
}

fn workspace_member_for_url<'a>(
    workspace_info: &'a pcb_zen::WorkspaceInfo,
    url: &str,
) -> Option<&'a str> {
    workspace_info
        .packages
        .keys()
        .filter(|package_url| package_url_covers(package_url, url))
        .max_by_key(|package_url| package_url.len())
        .map(|package_url| package_url.as_str())
}

fn resolve_kicad_url(
    url: &str,
    configured_kicad_versions: &BTreeMap<String, semver::Version>,
) -> Option<(String, DependencySpec)> {
    configured_kicad_versions
        .iter()
        .find_map(|(repo, version)| {
            package_url_covers(repo, url)
                .then(|| (repo.clone(), DependencySpec::Version(version.to_string())))
        })
}

fn extract_imports(content: &str) -> Option<CollectedImports> {
    let mut dialect = Dialect::Extended;
    dialect.enable_f_strings = true;

    let ast = AstModule::parse("<memory>", content.to_owned(), &dialect).ok()?;
    let mut result = CollectedImports::default();

    ast.statement().visit_expr(|expr| {
        visit_string_literals(expr, &mut |s, _| extract_from_str(s, &mut result));
    });

    for stmt in top_level_stmts(ast.statement()) {
        if let StmtP::Load(load) = &stmt.node {
            extract_from_str(&load.module.node, &mut result);
        }
    }

    Some(result)
}

fn extract_from_str(s: &str, result: &mut CollectedImports) {
    if let Some(spec) = LoadSpec::parse(s) {
        match spec {
            LoadSpec::Stdlib { .. } | LoadSpec::PackageUri { .. } => {}
            LoadSpec::Package { package, .. } => {
                result.aliases.insert(package);
            }
            LoadSpec::Github { .. } | LoadSpec::Gitlab { .. } => {
                result.urls.insert(s.to_string());
            }
            LoadSpec::Path { path, .. } => {
                result.relative_paths.push(path);
            }
        }
    }
}
