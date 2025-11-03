use std::io::Read;
use std::path::Path;

/// Load and decompress a zstd-compressed test file
///
/// All test XML files in tests/data are stored as `.xml.zst` files to save space.
/// This helper function transparently decompresses them for testing.
///
/// Space savings: ~200MB uncompressed → ~8MB compressed (96% reduction)
pub fn load_compressed_xml(path: &Path) -> String {
    let compressed_path = path.with_extension("xml.zst");
    let file = std::fs::File::open(&compressed_path)
        .unwrap_or_else(|_| panic!("Failed to open compressed test file: {:?}", compressed_path));

    let mut decoder = zstd::Decoder::new(file).expect("Failed to create zstd decoder");
    let mut content = String::new();
    decoder
        .read_to_string(&mut content)
        .expect("Failed to decompress test file");

    content
}

/// Parse an IPC-2581 file from compressed test data
///
/// Convenience wrapper that loads compressed test data and parses it.
pub fn parse_compressed(path: &str) -> Result<ipc2581::Ipc2581, ipc2581::Ipc2581Error> {
    let content = load_compressed_xml(Path::new(path));
    ipc2581::Ipc2581::parse(&content)
}
