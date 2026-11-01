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
    decode_zstd_stream(&decoded).map_err(|err| format!("invalid zstd data: {err}"))
}

/// Decode every frame of a Zstandard stream, skipping skippable frames and
/// validating each frame's content size and checksum when present.
/// Requires at least one data frame; unlike [`decode_embedded_payload`], this
/// does not accept uncompressed file bytes.
pub fn decode_zstd_stream(mut input: &[u8]) -> Result<Vec<u8>, String> {
    use ruzstd::decoding::StreamingDecoder;
    use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};

    let mut out = Vec::new();
    let mut decoded = false;
    while !decoded || !input.is_empty() {
        let header = input;
        let mut decoder = match StreamingDecoder::new(&mut input) {
            Ok(decoder) => decoder,
            Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                length,
                ..
            })) => {
                input = input
                    .get(length as usize..)
                    .ok_or_else(|| "truncated skippable frame".to_owned())?;
                continue;
            }
            Err(err) => return Err(err.to_string()),
        };
        // content_size() is zero both for an absent and an empty size; the
        // descriptor's size flag or single-segment bit says one is present.
        let expected = (header[4] & 0xe0 != 0).then(|| decoder.decoder.content_size());
        let start = out.len();
        // The size is untrusted, so one that cannot be reserved is ignored.
        let _ = out.try_reserve_exact(expected.unwrap_or(0) as usize);
        decoder
            .read_to_end(&mut out)
            .map_err(|err| err.to_string())?;
        if expected.is_some_and(|size| size != (out.len() - start) as u64) {
            return Err("content size mismatch".to_owned());
        }
        let frame = &decoder.decoder;
        if frame
            .get_checksum_from_data()
            .is_some_and(|sum| frame.get_calculated_checksum() != Some(sum))
        {
            return Err("checksum mismatch".to_owned());
        }
        decoded = true;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_zstd_and_plain_payloads() {
        let mut compressed = ruzstd::encoding::compress_to_vec(
            &b"step model"[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        );
        compressed.extend(ruzstd::encoding::compress_to_vec(
            &b" tail"[..],
            ruzstd::encoding::CompressionLevel::Fastest,
        ));
        let encoded = base64::engine::general_purpose::STANDARD.encode(compressed);
        let wrapped = format!("{}\n\t {}", &encoded[..8], &encoded[8..]);
        assert_eq!(
            decode_embedded_payload(wrapped.as_bytes()).unwrap(),
            b"step model tail"
        );
        assert_eq!(decode_embedded_payload(b"SGVs bG8=").unwrap(), b"Hello");
    }
}
