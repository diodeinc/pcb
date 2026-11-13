//! KiCad embedded-file payloads.

use base64::Engine;

/// Decode the payload of an `(embedded_files (file (data |...|)))` entry,
/// without the `|` delimiters: base64 with any ASCII whitespace, of a zstd
/// frame as KiCad writes it or of the plain file bytes.
pub fn decode_embedded_payload(payload: &[u8]) -> Result<Vec<u8>, String> {
    let encoded: Vec<u8> = payload
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|err| format!("invalid base64 data: {err}"))?;
    if !decoded.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Ok(decoded);
    }
    decode_zstd_stream(&decoded).map_err(|err| format!("invalid zstd data: {err}"))
}

/// Decode every frame of a Zstandard stream, skipping skippable frames and
/// validating each frame's content size and checksum when present.
/// Requires at least one data frame; unlike [`decode_embedded_payload`], this
/// does not accept uncompressed file bytes.
pub fn decode_zstd_stream(mut input: &[u8]) -> Result<Vec<u8>, String> {
    // zrip decodes a stream without a data frame to nothing, so find one.
    while let [0x50..=0x5f, 0x2a, 0x4d, 0x18, a, b, c, d, rest @ ..] = input {
        let length = u32::from_le_bytes([*a, *b, *c, *d]) as usize;
        input = rest.get(length..).ok_or("truncated skippable frame")?;
    }
    if input.is_empty() {
        return Err("no zstd frame".to_owned());
    }
    zrip::decompress(input).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_zstd_and_plain_payloads() {
        let mut compressed = zrip::compress(b"step model", 1).unwrap();
        compressed.extend(zrip::compress(b" tail", 1).unwrap());
        let encoded = base64::engine::general_purpose::STANDARD.encode(compressed);
        let wrapped = format!("{}\n\t {}", &encoded[..8], &encoded[8..]);
        assert_eq!(
            decode_embedded_payload(wrapped.as_bytes()).unwrap(),
            b"step model tail"
        );
        assert_eq!(decode_embedded_payload(b"SGVs bG8=").unwrap(), b"Hello");
    }
}
