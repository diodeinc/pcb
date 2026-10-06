use pcb_ir::geom::Resolution;
#[cfg(feature = "cli")]
use std::fs;
#[cfg(feature = "cli")]
use std::io::BufWriter;
use std::io::{Cursor, Seek, Write};
#[cfg(feature = "cli")]
use std::path::Path;
use std::path::PathBuf;

use anyhow::{Context, Result};
use pcb_ir::dialects::ipc::ArtworkScope;
use pcb_ir::import::ipc2581::ImportedDesign;
#[cfg(feature = "cli")]
use pcb_ir::import::ipc2581::import_design;
use zip::{ZipWriter, write::FileOptions};

use crate::gerber;
#[cfg(feature = "cli")]
use crate::ipc2581 as ipc;

#[derive(Debug, Clone)]
pub struct ManufacturingExportOptions {
    pub view: ArtworkScope,
    /// Include assembly, fabrication drawing, glue, courtyard, and document layers.
    pub include_auxiliary_layers: bool,
    /// Write the V-score relief construction as debug SVGs into this
    /// directory.
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
    pub contents: String,
}

/// Every Gerber X2 layer plus the XNC drill files for one artwork scope.
pub fn build_manufacturing_package(
    imported: &ImportedDesign,
    options: &ManufacturingExportOptions,
    resolution: Resolution,
) -> Result<ManufacturingPackage> {
    let mut files = gerber::build_gerber_x2_files(
        imported,
        options.view,
        &gerber::GerberExportOptions {
            relief_debug_dir: options.relief_debug_dir.clone(),
            include_auxiliary_layers: options.include_auxiliary_layers,
        },
        resolution,
    )?
    .into_iter()
    .map(|file| ManufacturingFile {
        filename: file.filename,
        contents: file.contents,
    })
    .collect::<Vec<_>>();
    files.extend(super::drill::build_xnc_drill_files_from_design(
        imported,
        options.view,
    )?);

    Ok(ManufacturingPackage { files })
}

/// Parse, import, build, and write a manufacturing package to `output`: a
/// zip when the path has a `.zip` extension, otherwise a directory.
#[cfg(feature = "cli")]
pub fn export_manufacturing_package(
    input_file: &Path,
    output: &Path,
    options: &ManufacturingExportOptions,
    resolution: Resolution,
) -> Result<ManufacturingPackage> {
    let content = crate::utils::file::load_ipc_file(input_file)?;
    let ipc = ipc::Ipc2581::parse(&content)?;
    let package =
        build_manufacturing_package(&import_design(&ipc, resolution)?, options, resolution)?;
    write_manufacturing_package(&package, output)?;
    Ok(package)
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
    // An opt-out export must not succeed with older auxiliary Gerbers still
    // present. Refuse before writing anything rather than delete user files.
    for entry in fs::read_dir(output_dir)? {
        let path = entry?.path();
        let auxiliary_extension =
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ["gbr", "gta", "gba"]
                        .iter()
                        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
                });
        if auxiliary_extension {
            anyhow::ensure!(
                package
                    .files
                    .iter()
                    .any(|file| path.file_name() == Some(file.filename.as_ref())),
                "output directory contains a Gerber not in this package: {}; use a fresh directory or a ZIP output",
                path.display()
            );
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[cfg(feature = "cli")]
    #[test]
    fn directory_rejects_stale_auxiliary_files_without_changing_existing_files() {
        for name in [
            "F_Fab.gbr",
            "F_Adhesive.gta",
            "B_Adhesive.gba",
            "Custom.GBR",
        ] {
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join("gerbers");
            let mut package = ManufacturingPackage {
                files: vec![
                    ManufacturingFile {
                        filename: "F_Cu.gtl".into(),
                        contents: "old copper".into(),
                    },
                    ManufacturingFile {
                        filename: name.into(),
                        contents: "drawing".into(),
                    },
                ],
            };
            write_manufacturing_package(&package, &output).unwrap();
            fs::write(output.join("notes.txt"), "keep me").unwrap();
            // Re-exporting the same file set remains supported.
            write_manufacturing_package(&package, &output).unwrap();
            let zip = directory.path().join("gerbers.zip");
            write_manufacturing_package(&package, &zip).unwrap();
            package.files.pop();
            package.files[0].contents = "new copper".into();
            let error = write_manufacturing_package(&package, &output).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("use a fresh directory or a ZIP output")
            );
            assert_eq!(
                fs::read_to_string(output.join("F_Cu.gtl")).unwrap(),
                "old copper"
            );
            assert_eq!(fs::read_to_string(output.join(name)).unwrap(), "drawing");
            assert_eq!(
                fs::read_to_string(output.join("notes.txt")).unwrap(),
                "keep me"
            );
            // ZIP replacement does remove the old entry.
            write_manufacturing_package(&package, &zip).unwrap();
            let mut archive = zip::ZipArchive::new(fs::File::open(zip).unwrap()).unwrap();
            assert_eq!(archive.len(), 1);
            assert_eq!(archive.by_index(0).unwrap().name(), "F_Cu.gtl");
            fs::remove_file(output.join(name)).unwrap();
            write_manufacturing_package(&package, &output).unwrap();
            assert_eq!(
                fs::read_to_string(output.join("F_Cu.gtl")).unwrap(),
                "new copper"
            );
        }
    }

    #[test]
    fn in_memory_zip_preserves_every_filename_and_contents() {
        let package = ManufacturingPackage {
            files: vec![
                ManufacturingFile {
                    filename: "PTH.drl".to_owned(),
                    contents: "M48\nMETRIC\nT01C0.6\n%\nT01\nX1.0Y2.0\nM30\n".to_owned(),
                },
                ManufacturingFile {
                    filename: "NPTH.drl".to_owned(),
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
