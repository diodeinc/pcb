use pcb_ir::geom::GeometryAccuracy;
#[cfg(feature = "cli")]
use std::fs;
#[cfg(feature = "cli")]
use std::io::BufWriter;
use std::io::{Cursor, Seek, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gerberx2::GerberLayer;
use ipc2581::Ipc2581;
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::import::ipc2581::{ImportedDesign, import_design};
use zip::{ZipWriter, write::FileOptions};

use crate::gerber;
#[cfg(feature = "cli")]
use crate::ipc2581 as ipc;

#[derive(Debug, Clone)]
pub struct ManufacturingExportOptions {
    pub output: PathBuf,
    pub view: ArtworkScope,
    pub relief_debug_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ManufacturingPackage {
    pub files: Vec<ManufacturingFile>,
}

impl ManufacturingPackage {
    /// Serialize the complete manufacturing package to an in-memory ZIP archive.
    pub fn to_zip(&self) -> Result<Vec<u8>> {
        Ok(write_zip(self, Cursor::new(Vec::new()))?.into_inner())
    }
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

#[cfg(feature = "cli")]
pub fn export_manufacturing_package(
    ipc: &Ipc2581,
    options: &ManufacturingExportOptions,
    accuracy: GeometryAccuracy,
) -> Result<ManufacturingPackage> {
    let package = build_manufacturing_package_with_options(ipc, options, accuracy)?;
    write_manufacturing_package(&package, &options.output)?;
    Ok(package)
}

pub fn build_manufacturing_package(
    ipc: &Ipc2581,
    view: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> Result<ManufacturingPackage> {
    let imported = import_design(ipc, accuracy)?;
    build_manufacturing_package_from_design(&imported, view, accuracy)
}

/// Export an already-imported design without repeating IPC ingestion.
pub fn build_manufacturing_package_from_design(
    imported: &ImportedDesign,
    view: ArtworkScope,
    accuracy: GeometryAccuracy,
) -> Result<ManufacturingPackage> {
    build_manufacturing_package_inner(imported, view, None, accuracy)
}

pub fn build_manufacturing_package_with_options(
    ipc: &Ipc2581,
    options: &ManufacturingExportOptions,
    accuracy: GeometryAccuracy,
) -> Result<ManufacturingPackage> {
    let imported = import_design(ipc, accuracy)?;
    build_manufacturing_package_inner(
        &imported,
        options.view,
        options.relief_debug_dir.as_deref(),
        accuracy,
    )
}

fn build_manufacturing_package_inner(
    imported: &ImportedDesign,
    view: ArtworkScope,
    relief_debug_dir: Option<&Path>,
    accuracy: GeometryAccuracy,
) -> Result<ManufacturingPackage> {
    let mut files = gerber::build_gerber_x2_files_from_design_with_options(
        imported,
        view,
        &gerber::GerberExportOptions {
            relief_debug_dir: relief_debug_dir.map(Path::to_path_buf),
        },
        accuracy,
    )?
    .into_iter()
    .map(|file| ManufacturingFile {
        filename: file.filename,
        kind: ManufacturingFileKind::GerberX2(file.layer),
        contents: file.contents,
    })
    .collect::<Vec<_>>();
    files.extend(super::drill::build_xnc_drill_files_from_design(
        imported, view, accuracy,
    )?);

    Ok(ManufacturingPackage { files })
}

#[cfg(feature = "cli")]
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

#[cfg(feature = "cli")]
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

#[cfg(feature = "cli")]
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
    write_zip(package, BufWriter::new(zip_file))?;
    Ok(())
}

fn write_zip<W: Write + Seek>(package: &ManufacturingPackage, writer: W) -> Result<W> {
    let mut zip = ZipWriter::new(writer);
    for file in &package.files {
        zip.start_file(&file.filename, FileOptions::<()>::default())
            .with_context(|| format!("failed to add {} to manufacturing zip", file.filename))?;
        zip.write_all(file.contents.as_bytes())
            .with_context(|| format!("failed to write {} to manufacturing zip", file.filename))?;
    }
    zip.finish().context("failed to finalize manufacturing zip")
}

#[cfg(feature = "cli")]
pub fn execute_file_with_options(
    input_file: &Path,
    options: &ManufacturingExportOptions,
    accuracy: GeometryAccuracy,
) -> Result<ManufacturingPackage> {
    let content = crate::utils::file::load_ipc_file(input_file)?;
    let ipc = ipc::Ipc2581::parse(&content)?;
    export_manufacturing_package(&ipc, options, accuracy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn in_memory_zip_preserves_every_filename_and_contents() {
        let package = ManufacturingPackage {
            files: vec![
                ManufacturingFile {
                    filename: "PTH.drl".to_owned(),
                    kind: ManufacturingFileKind::Xnc,
                    contents: "M48\nMETRIC\nT01C0.6\n%\nT01\nX1.0Y2.0\nM30\n".to_owned(),
                },
                ManufacturingFile {
                    filename: "NPTH.drl".to_owned(),
                    kind: ManufacturingFileKind::Xnc,
                    contents: "M48\nMETRIC\nT01C2.0\n%\nT01\nX3.0Y4.0\nM30\n".to_owned(),
                },
            ],
        };
        let zipped = package.to_zip().unwrap();
        assert_eq!(zipped, package.to_zip().unwrap());
        let mut archive = zip::ZipArchive::new(Cursor::new(zipped)).unwrap();
        assert_eq!(archive.len(), package.files.len());
        for file in &package.files {
            let mut restored = String::new();
            archive
                .by_name(&file.filename)
                .unwrap()
                .read_to_string(&mut restored)
                .unwrap();
            assert_eq!(restored, file.contents);
        }
    }
}
