use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};

/// XML text from already-read IPC bytes, decompressing `.zst` inputs.
pub fn ipc_text<'a>(path: &Path, bytes: &'a [u8]) -> Result<std::borrow::Cow<'a, str>> {
    if path.extension().and_then(|s| s.to_str()) == Some("zst") {
        let decoded = zstd::decode_all(bytes).context("Failed to decompress file")?;
        Ok(std::borrow::Cow::Owned(
            String::from_utf8(decoded).context("Decompressed file is not UTF-8")?,
        ))
    } else {
        Ok(std::borrow::Cow::Borrowed(
            std::str::from_utf8(bytes).with_context(|| format!("File is not UTF-8: {path:?}"))?,
        ))
    }
}

pub fn load_ipc_file(path: &Path) -> Result<String> {
    if path.extension().and_then(|s| s.to_str()) == Some("zst") {
        // Decompress zstd file
        let file = std::fs::File::open(path)
            .with_context(|| format!("Failed to open compressed file: {:?}", path))?;

        let mut decoder = zstd::Decoder::new(file).context("Failed to create zstd decoder")?;
        let mut content = String::new();
        decoder
            .read_to_string(&mut content)
            .context("Failed to decompress file")?;

        Ok(content)
    } else {
        // Read plain XML
        std::fs::read_to_string(path).with_context(|| format!("Failed to read file: {:?}", path))
    }
}

/// Save an IPC-2581 file, automatically compressing if path has .zst extension
pub fn save_ipc_file(path: &Path, content: &str) -> Result<()> {
    if path.extension().and_then(|s| s.to_str()) == Some("zst") {
        // Compress with zstd
        let file = std::fs::File::create(path)
            .with_context(|| format!("Failed to create compressed file: {:?}", path))?;

        let mut encoder = zstd::Encoder::new(file, 3).context("Failed to create zstd encoder")?;
        encoder
            .write_all(content.as_bytes())
            .context("Failed to write compressed data")?;
        encoder.finish().context("Failed to finish compression")?;

        Ok(())
    } else {
        // Write plain XML
        std::fs::write(path, content).with_context(|| format!("Failed to write file: {:?}", path))
    }
}
