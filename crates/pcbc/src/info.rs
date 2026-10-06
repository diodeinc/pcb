use anyhow::Result;
use clap::Args;
use colored::Colorize as ColoredExt;
use pcb_eda::kicad::symbol_library::KicadSymbolLibrary;
use pcb_ui::{Style, StyledText};
use pcb_zen::workspace::{DiscoveryError, SymbolFileInfo, WorkspaceInfo, WorkspacePackage};
use pcb_zen_core::config::PcbToml;
use pcb_zen_core::resolution::ResolutionResult;
use rayon::prelude::*;
use serde::Serialize;
use std::collections::BTreeMap;
use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
#[command(about = "Display workspace and board information")]
pub struct InfoArgs {
    /// Output format
    #[arg(short = 'f', long, value_enum, default_value = "human")]
    pub format: OutputFormat,

    /// Optional path to start discovery from (defaults to current directory)
    pub path: Option<String>,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable output
    Human,
    /// JSON output
    Json,
}

#[derive(Debug, Default, Serialize)]
struct InfoJson {
    #[serde(skip_serializing_if = "Option::is_none")]
    root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<PcbToml>,
    packages: BTreeMap<String, PackageMetadata>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    external_dependencies: BTreeMap<String, PackageMetadata>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    errors: Vec<DiscoveryError>,
}

#[derive(Debug, Serialize)]
struct PackageMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    rel_path: PathBuf,
    source: PackageSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    board_entrypoint: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    config: PcbToml,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_at: Option<String>,
    #[serde(default)]
    preferred: bool,
    #[serde(default, skip_serializing_if = "is_default")]
    dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    entrypoints: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    symbol_files: Vec<SymbolFileInfo>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum PackageSource {
    Workspace,
    Vendor,
    Cache,
    Patch,
    Other,
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

pub fn execute(args: InfoArgs) -> Result<()> {
    let start_path = match &args.path {
        Some(path) => Path::new(path).to_path_buf(),
        None => env::current_dir()?,
    };

    match args.format {
        OutputFormat::Human => {
            let resolution = crate::resolve::resolve(Some(&start_path), false)?;
            let mut workspace_info = resolution.workspace_info.clone();
            pcb_zen::workspace::enrich_git_metadata(&mut workspace_info);
            let mut errors = Vec::new();
            let external_dependencies =
                external_dependencies(&workspace_info, &resolution, &mut errors);
            if let Some(error) = errors.first() {
                anyhow::bail!("{}: {}", error.path.display(), error.error);
            }
            pcb_ui::write_stdout(|stdout| {
                print_human_readable(stdout, &workspace_info, &external_dependencies)
            })?;
        }
        OutputFormat::Json => {
            let info = info_json(&start_path);
            let json = serde_json::to_string_pretty(&info)?;
            pcb_ui::write_stdout(|stdout| writeln!(stdout, "{json}"))?;
            anyhow::ensure!(info.root.is_some(), "Could not discover workspace root");
        }
    }

    Ok(())
}

fn info_json(start_path: &Path) -> InfoJson {
    let mut errors = Vec::new();
    let provider = pcb_zen_core::DefaultFileProvider::new();
    let workspace = pcb_zen::get_workspace_info(&provider, start_path).or_else(|error| {
        let Some(failure) = error.downcast_ref::<DiscoveryError>() else {
            return Err(error);
        };
        let Some(root) = failure.path.ancestors().skip(2).find(|dir| {
            PcbToml::from_path(&dir.join("pcb.toml")).is_ok_and(|config| config.is_workspace())
        }) else {
            return Err(error);
        };
        let mut ws = pcb_zen::get_workspace_info(&provider, root)?;
        if !ws.errors.iter().any(|err| err.path == failure.path) {
            ws.errors.push(failure.clone());
        }
        Ok(ws)
    });
    let Some(mut ws) = best_effort(workspace, start_path, &mut errors) else {
        return InfoJson {
            errors,
            ..InfoJson::default()
        };
    };
    errors.append(&mut ws.errors);
    let external_dependencies = if errors.is_empty() {
        best_effort(
            pcb_zen::resolve_workspace_dependencies(ws.clone(), start_path, false),
            start_path,
            &mut errors,
        )
        .map(|resolution| {
            external_dependencies(&resolution.workspace_info, &resolution, &mut errors)
        })
        .unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    pcb_zen::workspace::enrich_git_metadata(&mut ws);
    populate_package_file_discovery(&mut ws);
    errors.append(&mut ws.errors);
    let packages = ws
        .packages
        .iter()
        .map(|(module_path, pkg)| {
            (
                module_path.clone(),
                metadata_for_workspace_package(&ws, module_path, pkg),
            )
        })
        .collect();

    InfoJson {
        root: Some(ws.root),
        config: ws.config,
        packages,
        external_dependencies,
        errors,
    }
}

fn metadata_for_workspace_package(
    ws: &WorkspaceInfo,
    module_path: &str,
    pkg: &WorkspacePackage,
) -> PackageMetadata {
    PackageMetadata {
        version: pkg.version.clone(),
        rel_path: pkg.rel_path.clone(),
        source: if is_path_patch(ws, module_path, &pkg.dir(&ws.root)) {
            PackageSource::Patch
        } else {
            PackageSource::Workspace
        },
        board_entrypoint: pkg
            .config
            .board
            .as_ref()
            .and_then(|board| board.path.as_ref())
            .and_then(|path| pathdiff::diff_paths(pkg.dir(&ws.root).join(path), &ws.root)),
        tag_prefix: Some(pcb_zen::tags::compute_tag_prefix(
            Some(&pkg.rel_path),
            ws.path(),
        )),
        config: pkg.config.clone(),
        published_at: pkg.published_at.clone(),
        preferred: pkg.preferred,
        dirty: pkg.dirty,
        content_hash: None,
        manifest_hash: None,
        entrypoints: pkg.entrypoints.clone(),
        symbol_files: pkg.symbol_files.clone(),
    }
}

fn external_dependencies(
    ws: &WorkspaceInfo,
    resolution: &ResolutionResult,
    errors: &mut Vec<DiscoveryError>,
) -> BTreeMap<String, PackageMetadata> {
    let mut deps = BTreeMap::new();
    let package_roots = resolution.package_roots();

    for (coord, root) in package_roots {
        let Some((module_path, version)) = external_package_coord(ws, &coord, &root) else {
            continue;
        };
        let manifest_path = root.join("pcb.toml");
        if !manifest_path.exists() {
            continue;
        }

        let config = best_effort(PcbToml::from_path(&manifest_path), &manifest_path, errors)
            .unwrap_or_default();
        let (entrypoints, symbol_files) = discover_package_files(&root, errors);
        let module_path = module_path.to_string();
        let version = version.to_string();

        deps.insert(
            coord,
            PackageMetadata {
                version: Some(version),
                rel_path: dependency_rel_path(ws, &root),
                source: package_source(ws, &module_path, &root),
                board_entrypoint: None,
                tag_prefix: None,
                config,
                published_at: None,
                preferred: false,
                dirty: false,
                content_hash: None,
                manifest_hash: None,
                entrypoints,
                symbol_files,
            },
        );
    }

    deps
}

fn dependency_rel_path(ws: &WorkspaceInfo, root: &Path) -> PathBuf {
    let workspace_cache = ws.workspace_cache_dir();
    if let Some(rel) = strip_prefix(root, &workspace_cache) {
        return PathBuf::from(".pcb/cache").join(rel);
    }
    if let Some(rel) = strip_prefix(root, &ws.cache_dir) {
        return PathBuf::from(".pcb/cache").join(rel);
    }
    root.strip_prefix(&ws.root).unwrap_or(root).to_path_buf()
}

fn strip_prefix(path: &Path, base: &Path) -> Option<PathBuf> {
    if base.as_os_str().is_empty() {
        return None;
    }
    path.strip_prefix(base)
        .map(Path::to_path_buf)
        .ok()
        .or_else(|| {
            let path = canonical_or_self(path);
            let base = canonical_or_self(base);
            path.strip_prefix(base).map(Path::to_path_buf).ok()
        })
}

fn external_package_coord<'a>(
    ws: &WorkspaceInfo,
    coord: &'a str,
    root: &Path,
) -> Option<(&'a str, &'a str)> {
    let (module_path, version) = coord.rsplit_once('@')?;
    if module_path.is_empty()
        || version.is_empty()
        || ws.packages.contains_key(module_path)
        || pcb_zen_core::is_stdlib_module_path(module_path)
        || root.file_name().is_none_or(|name| name != version)
    {
        return None;
    }
    Some((module_path, version))
}

fn package_source(ws: &WorkspaceInfo, module_path: &str, root: &Path) -> PackageSource {
    if is_path_patch(ws, module_path, root) {
        return PackageSource::Patch;
    }
    let root = canonical_or_self(root);
    if root.starts_with(canonical_or_self(&ws.root.join("vendor"))) {
        return PackageSource::Vendor;
    }
    if root.starts_with(canonical_or_self(&ws.workspace_cache_dir()))
        || root.starts_with(canonical_or_self(&ws.cache_dir))
    {
        return PackageSource::Cache;
    }
    PackageSource::Other
}

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn is_path_patch(ws: &WorkspaceInfo, module_path: &str, root: &Path) -> bool {
    ws.config
        .as_ref()
        .and_then(|config| config.patch.get(module_path))
        .and_then(|patch| patch.path.as_ref())
        .is_some_and(|path| canonical_or_self(&ws.root.join(path)) == canonical_or_self(root))
}

fn populate_package_file_discovery(ws: &mut WorkspaceInfo) {
    let root = &ws.root;
    let errors: Vec<_> = pcb_zen_core::workspace::with_readers(|| {
        ws.packages
            .par_iter_mut()
            .map(|(_, pkg)| {
                let mut errors = Vec::new();
                (pkg.entrypoints, pkg.symbol_files) =
                    discover_package_files(&pkg.dir(root), &mut errors);
                errors
            })
            .collect()
    });
    ws.errors.extend(errors.into_iter().flatten());
}

fn discover_package_files(
    package_dir: &Path,
    errors: &mut Vec<DiscoveryError>,
) -> (Vec<PathBuf>, Vec<SymbolFileInfo>) {
    let Some(entries) = best_effort(std::fs::read_dir(package_dir), package_dir, errors) else {
        return (Vec::new(), Vec::new());
    };
    let mut entries: Vec<_> = entries
        .filter_map(|entry| best_effort(entry, package_dir, errors))
        .collect();
    entries.sort_by_key(|entry| entry.file_name());

    let mut entrypoints = Vec::new();
    let mut symbol_files = Vec::new();

    for entry in entries {
        let path = entry.path();
        let extension = path.extension().and_then(|ext| ext.to_str());
        if !matches!(extension, Some("zen" | "kicad_sym")) {
            continue;
        }

        if !best_effort(entry.file_type(), &path, errors).is_some_and(|kind| kind.is_file()) {
            continue;
        }

        let rel_path = PathBuf::from(entry.file_name());
        match extension {
            Some("zen") => entrypoints.push(rel_path),
            Some("kicad_sym") => {
                let Some(library) =
                    best_effort(KicadSymbolLibrary::from_file(&path), &path, errors)
                else {
                    continue;
                };
                let symbols = library
                    .symbol_names()
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                symbol_files.push(SymbolFileInfo {
                    path: rel_path,
                    symbols,
                });
            }
            _ => {}
        }
    }

    (entrypoints, symbol_files)
}

fn best_effort<T, E: Into<anyhow::Error>>(
    result: Result<T, E>,
    path: &Path,
    errors: &mut Vec<DiscoveryError>,
) -> Option<T> {
    result
        .map_err(|error| {
            let error = error.into();
            errors.push(
                error
                    .downcast_ref::<DiscoveryError>()
                    .cloned()
                    .unwrap_or_else(|| DiscoveryError {
                        path: path.to_path_buf(),
                        error: format!("{error:#}"),
                    }),
            )
        })
        .ok()
}

fn print_human_readable(
    writer: &mut (impl Write + ?Sized),
    ws: &WorkspaceInfo,
    external_dependencies: &BTreeMap<String, PackageMetadata>,
) -> io::Result<()> {
    // Header
    writeln!(writer, "{}", "Workspace".with_style(Style::Blue).bold())?;
    writeln!(writer, "Root: {}", ws.root.display())?;

    if let Some(repo) = ws.repository() {
        writeln!(writer, "Repository: {}", repo.with_style(Style::Cyan))?;
    }
    if let Some(pcb_version) = ws.pcb_version() {
        writeln!(writer, "Toolchain: pcb >= {}", pcb_version)?;
    }

    writeln!(writer)?;

    // Separate boards from other packages
    let all_packages = ws.all_packages();
    let (mut boards, mut other_packages): (Vec<_>, Vec<_>) = all_packages
        .into_iter()
        .partition(|p| p.config.board.is_some());

    // Sort by relative path
    boards.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    other_packages.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // Boards section (like V1)
    if boards.is_empty() {
        writeln!(writer, "No boards discovered")?;
    } else {
        writeln!(
            writer,
            "{} ({})",
            "Boards".with_style(Style::Blue).bold(),
            boards.len()
        )?;

        for pkg in &boards {
            if let Some(board) = &pkg.config.board {
                // board.path is already populated by populate_board_zen_paths()
                let zen_path = board
                    .path
                    .as_ref()
                    .map(|p| {
                        // Make path relative to workspace root
                        let pkg_rel = pkg.rel_path.to_string_lossy();
                        if pkg_rel.is_empty() {
                            p.clone()
                        } else {
                            format!("{}/{}", pkg_rel, p)
                        }
                    })
                    .unwrap_or_else(|| "(no .zen file found)".to_string());

                // Use package version (which is board version for board packages)
                let version_str = format_version(&pkg.version, false);

                writeln!(
                    writer,
                    "  {} {} - {}",
                    board.name.bold(),
                    version_str,
                    zen_path
                )?;

                if !board.description.is_empty() {
                    writeln!(writer, "    {}", board.description)?;
                }
            }
        }
    }

    // Packages section (non-boards)
    if !other_packages.is_empty() {
        writeln!(writer)?;
        writeln!(
            writer,
            "{} ({})",
            "Packages".with_style(Style::Blue).bold(),
            other_packages.len()
        )?;

        for pkg in &other_packages {
            print_package_line(writer, pkg)?;
        }
    }

    if !external_dependencies.is_empty() {
        writeln!(writer)?;
        writeln!(
            writer,
            "{} ({})",
            "External dependencies".with_style(Style::Blue).bold(),
            external_dependencies.len()
        )?;

        for (coord, dep) in external_dependencies {
            print_external_dependency_line(writer, coord, dep)?;
        }
    }

    Ok(())
}

fn print_package_line(
    writer: &mut (impl Write + ?Sized),
    pkg: &WorkspacePackage,
) -> io::Result<()> {
    let is_root = pkg.rel_path.as_os_str().is_empty();

    // Package name (last segment of relative path, or "root")
    let name = if is_root {
        "root".to_string()
    } else {
        pkg.rel_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| pkg.rel_path.to_string_lossy().to_string())
    };

    let is_dirty = pkg.dirty;
    let version_str = format_version(&pkg.version, is_dirty);

    // Relative path from workspace root
    let rel_path = pkg.rel_path.to_string_lossy().to_string();

    // Dependency count suffix
    let dep_count = pkg.dependencies().count();
    let mut extras = Vec::new();
    if dep_count > 0 {
        extras.push(format!("{} deps", dep_count));
    }
    let extras_str = if extras.is_empty() {
        String::new()
    } else {
        format!(" ({})", extras.join(", ")).dimmed().to_string()
    };

    // Root indicator
    let root_str = if is_root {
        " (workspace root)".cyan().to_string()
    } else {
        String::new()
    };

    // Path display
    let path_str = if rel_path.is_empty() || is_root {
        String::new()
    } else {
        format!(" {}", rel_path.dimmed())
    };

    writeln!(
        writer,
        "  {} {}{}{}{}",
        name.bold(),
        version_str,
        root_str,
        path_str,
        extras_str
    )
}

fn print_external_dependency_line(
    writer: &mut (impl Write + ?Sized),
    coord: &str,
    dep: &PackageMetadata,
) -> io::Result<()> {
    let module_path = coord.rsplit_once('@').map_or(coord, |(path, _)| path);
    let version = dep.version.as_deref().unwrap_or("unknown");
    let source = dep.source.as_str();
    let path = dep.rel_path.to_string_lossy();

    writeln!(
        writer,
        "  {} {} {} {}",
        module_path.bold(),
        format!("(v{version})").green(),
        format!("[{source}]").dimmed(),
        path.dimmed()
    )
}

impl PackageSource {
    fn as_str(&self) -> &'static str {
        match self {
            PackageSource::Workspace => "workspace",
            PackageSource::Vendor => "vendor",
            PackageSource::Cache => "cache",
            PackageSource::Patch => "patch",
            PackageSource::Other => "other",
        }
    }
}

/// Format version string with dirty indicator
fn format_version(version: &Option<String>, dirty: bool) -> String {
    match (version, dirty) {
        (Some(v), true) => format!("{}{}", format!("(v{})", v).green(), "*".red()),
        (Some(v), false) => format!("(v{})", v).green().to_string(),
        (None, _) => "(unpublished)".yellow().to_string(),
    }
}
