//! WASI binary for testing pcb-zen-wasm evaluate logic.
//!
//! Usage: pcb-zen-wasi [zip_file] [main_file] [inputs_json]
//!
//! If zip_file is `-` or omitted, reads from stdin.
//! If main_file is omitted, auto-detects from boards/ directory.
//!
//! Accepts both "src zips" (source files at root) and "release zips"
//! (which have metadata.json at root and source files under src/).
//! Release zips are automatically detected and the src/ contents extracted.
//!
//! Example:
//!   cat src.zip | wasmtime run pcb-zen-wasi.wasm
//!   cat release.zip | wasmtime run pcb-zen-wasi.wasm
//!   cat src.zip | wasmtime run pcb-zen-wasi.wasm - boards/foo/foo.zen '{}'

use std::io::{Cursor, Read, copy};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

/// If the zip is a release zip (detected by presence of metadata.json),
/// extract only the src/ subdirectory contents. Otherwise return as-is.
fn maybe_extract_src(zip_bytes: Vec<u8>) -> Result<Vec<u8>, zip::result::ZipError> {
    let mut archive = ZipArchive::new(Cursor::new(&zip_bytes))?;

    // Not a release zip if no metadata.json
    if archive.by_name("metadata.json").is_err() {
        return Ok(zip_bytes);
    }

    eprintln!("Detected release zip, extracting src/ contents...");

    let mut output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(&mut output);
    let options = SimpleFileOptions::default();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(stripped) = file.name().strip_prefix("src/") else {
            continue;
        };
        if stripped.is_empty() {
            continue;
        }

        if file.is_dir() {
            writer.add_directory(stripped, options)?;
        } else {
            writer.start_file(stripped, options)?;
            copy(&mut file, &mut writer)?;
        }
    }

    writer.finish()?;
    Ok(output.into_inner())
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let zip_path = args.get(1).map(|s| s.as_str()).unwrap_or("-");
    let main_file = args.get(2).map(|s| s.as_str()).unwrap_or("");
    let inputs_json = args.get(3).map(|s| s.as_str()).unwrap_or("{}");

    let zip_bytes = if zip_path == "-" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).unwrap_or_else(|e| {
            eprintln!("Failed to read zip from stdin: {e}");
            std::process::exit(1);
        });
        buf
    } else {
        std::fs::read(zip_path).unwrap_or_else(|e| {
            eprintln!("Failed to read zip file '{zip_path}': {e}");
            std::process::exit(1);
        })
    };

    let zip_bytes = maybe_extract_src(zip_bytes).unwrap_or_else(|e| {
        eprintln!("Failed to process zip: {e}");
        std::process::exit(1);
    });

    let result =
        pcb_zen_wasm::evaluate_impl(zip_bytes, main_file, inputs_json).unwrap_or_else(|e| {
            eprintln!("Evaluation error: {e}");
            std::process::exit(1);
        });

    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if !result.success {
        std::process::exit(1);
    }
}
