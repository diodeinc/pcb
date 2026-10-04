use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pcb_ir::geom::Resolution;
use sha2::{Digest, Sha256};

use crate::LayoutTarget;
use crate::drawing::{FabDrawingOptions, Typeface, fab_drawing};
use crate::ipc2581::Ipc2581;
use crate::utils::file as file_utils;

#[derive(Debug, Clone)]
pub struct FabDrawingCommandOptions {
    pub output: PathBuf,
    pub target: LayoutTarget,
    /// The design's name, where the board step's own is not wanted.
    pub title: Option<String>,
    /// The revision, where the design's own is not wanted.
    pub revision: Option<String>,
}

pub fn execute(
    input_file: &Path,
    options: &FabDrawingCommandOptions,
    resolution: Resolution,
) -> Result<()> {
    let content = file_utils::load_ipc_file(input_file)?;
    let ipc = Ipc2581::parse(&content)?;
    let imported = pcb_ir::import::ipc2581::import_design(&ipc, resolution)?;
    let digest = hex::encode(Sha256::digest(content.as_bytes()));
    let source = input_file
        .file_name()
        .map(|name| format!("{}  ·  SHA-256 {}", name.to_string_lossy(), &digest[..12]));
    let (face, typeface) =
        installed_typeface().unwrap_or_else(|| ("Roboto Mono".to_string(), Typeface::default()));
    let drawing = FabDrawingOptions {
        target: options.target,
        title: options.title.clone(),
        revision: options.revision.clone(),
        source,
        typeface,
    };
    let pdf = fab_drawing(&ipc, &imported, &drawing, resolution)?;
    std::fs::write(&options.output, pdf)
        .with_context(|| format!("Failed to write PDF to {}", options.output.display()))?;
    println!(
        "✓ IPC-2581 fabrication drawing written to {} (lettered in {face})",
        options.output.display()
    );
    Ok(())
}

/// TX-02 is also sold as Berkeley Mono.
const FAMILIES: [&str; 2] = ["TX-02", "Berkeley Mono"];

fn font_directories() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let in_home = |path: &str| home.as_ref().map(|home| home.join(path));
    [
        std::env::var_os("PCB_FONT_DIR").map(PathBuf::from),
        in_home("Library/Fonts"),
        in_home(".local/share/fonts"),
        in_home(".fonts"),
        Some(PathBuf::from("/Library/Fonts")),
        Some(PathBuf::from("/usr/local/share/fonts")),
        Some(PathBuf::from("/usr/share/fonts")),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Font files in `directory` and the directories directly under it.
fn font_files(directory: &Path) -> Vec<PathBuf> {
    let list = |directory: &Path| {
        let entries = std::fs::read_dir(directory).into_iter().flatten();
        entries
            .filter_map(|entry| Some(entry.ok()?.path()))
            .collect::<Vec<_>>()
    };
    let is_font = |path: &PathBuf| {
        let extension = path
            .extension()
            .map(|extension| extension.to_ascii_lowercase());
        extension.is_some_and(|extension| extension == "ttf")
    };
    let mut files = list(directory)
        .into_iter()
        .flat_map(|path| {
            if path.is_dir() {
                list(&path)
            } else {
                vec![path]
            }
        })
        .filter(is_font)
        .collect::<Vec<_>>();
    files.sort();
    files
}

/// Upright regular and bold faces of one width, semi-condensed before normal.
fn installed_typeface() -> Option<(String, Typeface)> {
    use ttf_parser::name_id::{FAMILY, TYPOGRAPHIC_FAMILY};
    for directory in font_directories() {
        // A file name says nothing certain, so every font is asked its family.
        let mut faces = Vec::new();
        for path in font_files(&directory) {
            let Ok(data) = std::fs::read(&path) else {
                continue;
            };
            let Ok(face) = ttf_parser::Face::parse(&data, 0) else {
                continue;
            };
            let family = [TYPOGRAPHIC_FAMILY, FAMILY].into_iter().find_map(|id| {
                // A face may carry the name in several encodings.
                let names = face.names().into_iter();
                names
                    .filter(|name| name.name_id == id)
                    .find_map(|name| name.to_string())
            });
            let Some(family) = family.filter(|family| FAMILIES.contains(&family.as_str())) else {
                continue;
            };
            if face.is_italic() || face.tables().glyf.is_none() {
                continue;
            }
            let (width, weight) = (face.width().to_number(), face.weight().to_number());
            faces.push((family, width, weight, data));
        }
        // Semi-condensed (4) first, then normal (5), then whatever else.
        let mut widths = faces.iter().map(|face| face.1).collect::<Vec<_>>();
        widths.sort_by_key(|width| ((*width != 4), (*width != 5), *width));
        let found = widths.into_iter().find_map(|width| {
            let of_weight = |weight: u16| {
                let mut faces = faces.iter();
                faces.find(|face| face.1 == width && face.2 == weight)
            };
            let (regular, bold) = (of_weight(400)?, of_weight(700)?);
            let typeface = Typeface {
                regular: regular.3.clone().into(),
                bold: bold.3.clone().into(),
            };
            Some((regular.0.clone(), typeface))
        });
        // The first directory that has the family decides.
        if found.is_some() {
            return found;
        }
    }
    None
}
