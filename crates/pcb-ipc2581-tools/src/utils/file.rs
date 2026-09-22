use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// XML text from already-read IPC bytes. Zstandard input is recognized by
/// its frame magic, so a compressed file reads under any name.
pub fn ipc_text<'a>(path: &Path, bytes: &'a [u8]) -> Result<std::borrow::Cow<'a, str>> {
    if is_zstd(bytes) {
        let decoded = zstd::decode_all(bytes)
            .with_context(|| format!("Failed to decompress file: {path:?}"))?;
        Ok(std::borrow::Cow::Owned(
            String::from_utf8(decoded)
                .with_context(|| format!("Decompressed file is not UTF-8: {path:?}"))?,
        ))
    } else {
        Ok(std::borrow::Cow::Borrowed(
            std::str::from_utf8(bytes).with_context(|| format!("File is not UTF-8: {path:?}"))?,
        ))
    }
}

/// The one way commands read IPC-2581 input, plain or Zstandard-compressed.
pub fn load_ipc_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("Failed to read file: {path:?}"))?;
    Ok(ipc_text(path, &bytes)?.into_owned())
}

/// A Zstandard or skippable frame, either of which may open a `.zst` file.
fn is_zstd(bytes: &[u8]) -> bool {
    matches!(
        bytes,
        [0x28, 0xb5, 0x2f, 0xfd, ..] | [0x50..=0x5f, 0x2a, 0x4d, 0x18, ..]
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_plain_and_zstandard_input_under_any_name() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?><IPC-2581 revision="C"/>"#;
        let dir = tempfile::tempdir().unwrap();
        let compressed = zstd::encode_all(xml.as_bytes(), 0).unwrap();
        for (name, bytes) in [
            ("board.xml", xml.as_bytes()),
            ("board.xml.zst", &compressed),
            ("compressed-without-extension.xml", &compressed),
        ] {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(load_ipc_file(&path).unwrap(), xml, "{name}");
        }
    }

    #[test]
    fn saved_zst_files_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.xml.zst");
        save_ipc_file(&path, "<IPC-2581/>").unwrap();
        assert_ne!(std::fs::read(&path).unwrap(), b"<IPC-2581/>");
        assert_eq!(load_ipc_file(&path).unwrap(), "<IPC-2581/>");
    }
}
