use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use gerberx2::GerberLayer;
use ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::View;
use zip::{ZipWriter, write::FileOptions};

use crate::{gerber, ipc2581 as ipc};

#[derive(Debug, Clone)]
pub struct ManufacturingExportOptions {
    pub output: PathBuf,
    pub view: View,
    pub relief_debug_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ManufacturingPackage {
    pub files: Vec<ManufacturingFile>,
}

#[derive(Debug, Clone)]
pub struct ManufacturingFile {
    pub filename: String,
    pub kind: ManufacturingFileKind,
    pub contents: String,
}

#[derive(Debug, Clone)]
pub enum ManufacturingFileKind {
    GerberX2(GerberLayer),
    Xnc,
}

pub fn export_manufacturing_package(
    ipc: &Ipc2581,
    options: &ManufacturingExportOptions,
) -> Result<ManufacturingPackage> {
    let package = build_manufacturing_package_with_options(ipc, options)?;
    write_manufacturing_package(&package, &options.output)?;
    Ok(package)
}

pub fn build_manufacturing_package(ipc: &Ipc2581, view: View) -> Result<ManufacturingPackage> {
    build_manufacturing_package_inner(ipc, view, None)
}

pub fn build_manufacturing_package_with_options(
    ipc: &Ipc2581,
    options: &ManufacturingExportOptions,
) -> Result<ManufacturingPackage> {
    build_manufacturing_package_inner(ipc, options.view, options.relief_debug_dir.as_deref())
}

fn build_manufacturing_package_inner(
    ipc: &Ipc2581,
    view: View,
    relief_debug_dir: Option<&Path>,
) -> Result<ManufacturingPackage> {
    if view == View::LayoutSymbolic {
        bail!(
            "manufacturing export does not support symbolic layout view; use board or board-array"
        );
    }

    let mut files = gerber::build_gerber_x2_files_with_options(
        ipc,
        view,
        &gerber::GerberExportOptions {
            relief_debug_dir: relief_debug_dir.map(Path::to_path_buf),
        },
    )?
    .into_iter()
    .map(|file| ManufacturingFile {
        filename: file.filename,
        kind: ManufacturingFileKind::GerberX2(file.layer),
        contents: file.contents,
    })
    .collect::<Vec<_>>();
    files.extend(super::drill::build_xnc_drill_files(ipc, view)?);

    Ok(ManufacturingPackage { files })
}

pub fn write_manufacturing_package(package: &ManufacturingPackage, output: &Path) -> Result<()> {
    if output
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
    {
        write_manufacturing_zip(package, output)
    } else {
        write_manufacturing_directory(package, output)
    }
}

fn write_manufacturing_directory(package: &ManufacturingPackage, output_dir: &Path) -> Result<()> {
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create manufacturing output directory {}",
            output_dir.display()
        )
    })?;
    for file in &package.files {
        fs::write(output_dir.join(&file.filename), &file.contents).with_context(|| {
            format!(
                "failed to write manufacturing file {}",
                output_dir.join(&file.filename).display()
            )
        })?;
    }
    Ok(())
}

fn write_manufacturing_zip(package: &ManufacturingPackage, output_zip: &Path) -> Result<()> {
    if let Some(parent) = output_zip.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create manufacturing zip output directory {}",
                parent.display()
            )
        })?;
    }

    let zip_file = fs::File::create(output_zip).with_context(|| {
        format!(
            "failed to create manufacturing zip {}",
            output_zip.display()
        )
    })?;
    let mut zip = ZipWriter::new(BufWriter::new(zip_file));
    for file in &package.files {
        zip.start_file(&file.filename, FileOptions::<()>::default())
            .with_context(|| format!("failed to add {} to manufacturing zip", file.filename))?;
        zip.write_all(file.contents.as_bytes())
            .with_context(|| format!("failed to write {} to manufacturing zip", file.filename))?;
    }
    zip.finish().with_context(|| {
        format!(
            "failed to finalize manufacturing zip {}",
            output_zip.display()
        )
    })?;
    Ok(())
}

pub fn execute_file_with_options(
    input_file: &Path,
    options: &ManufacturingExportOptions,
) -> Result<ManufacturingPackage> {
    let content = crate::utils::file::load_ipc_file(input_file)?;
    let ipc = ipc::Ipc2581::parse(&content)?;
    export_manufacturing_package(&ipc, options)
}
