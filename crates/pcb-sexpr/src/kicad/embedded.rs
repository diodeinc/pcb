//! KiCad embedded-file payloads.

use std::io::Read;

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
    let zstd_error = |err: &dyn std::fmt::Display| format!("invalid zstd data: {err}");
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(decoded.as_slice())
        .map_err(|err| zstd_error(&err))?;
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|err| zstd_error(&err))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_zstd_and_plain_payloads() {
        let compressed = ruzstd::encoding::compress_to_vec(
            &b"step model"[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(compressed);
        let wrapped = format!("{}\n\t {}", &encoded[..8], &encoded[8..]);
        assert_eq!(
            decode_embedded_payload(wrapped.as_bytes()).unwrap(),
            b"step model"
        );
        assert_eq!(decode_embedded_payload(b"SGVs bG8=").unwrap(), b"Hello");
    }
}
