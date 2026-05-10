use std::fmt;

use base64::Engine;
use sha2::{Digest, Sha256};

use crate::{Sexpr, Span, find_all_child_lists, find_child_list};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintValidationIssue {
    pub message: String,
    pub span: Option<Span>,
}

impl FootprintValidationIssue {
    fn new(message: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintValidationError {
    pub issues: Vec<FootprintValidationIssue>,
}

impl FootprintValidationError {
    fn new(issue: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            issues: vec![FootprintValidationIssue::new(issue, span)],
        }
    }
}

impl fmt::Display for FootprintValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (idx, issue) in self.issues.iter().enumerate() {
            if idx > 0 {
                writeln!(f)?;
            }
            write!(f, "{}", issue.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for FootprintValidationError {}

/// Validate a KiCad footprint source file.
///
/// In addition to basic S-expression shape checks, this verifies KiCad 10
/// embedded-file checksums. KiCad stores embedded payloads as base64-encoded
/// zstd data and records a KiCad-specific MMH3 hash of the decompressed original
/// bytes. Older files may use a SHA-256 hash.
pub fn validate_footprint_source(source: &str) -> Result<(), FootprintValidationError> {
    let parsed = crate::parse(source).map_err(|err| {
        FootprintValidationError::new(format!("Invalid footprint S-expression: {err}"), None)
    })?;
    let Some(root) = parsed.as_list() else {
        return Err(FootprintValidationError::new(
            "Invalid footprint S-expression: root must be a list",
            Some(parsed.span),
        ));
    };
    if !matches!(
        root.first().and_then(Sexpr::as_sym),
        Some("footprint" | "module")
    ) {
        return Err(FootprintValidationError::new(
            "Invalid footprint S-expression: root list must start with `footprint` or legacy `module`",
            Some(parsed.span),
        ));
    }

    let mut issues = Vec::new();
    for embedded_files in find_all_child_lists(root, "embedded_files") {
        for file in find_all_child_lists(embedded_files, "file") {
            validate_embedded_file(file, &mut issues);
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(FootprintValidationError { issues })
    }
}

fn validate_embedded_file(file: &[Sexpr], issues: &mut Vec<FootprintValidationIssue>) {
    let file_span = list_span(file);
    let name = find_child_list(file, "name")
        .and_then(|list| collect_child_atom_text(list, 1))
        .unwrap_or_else(|| "<unnamed>".to_string());

    let Some(data_list) = find_child_list(file, "data") else {
        return;
    };
    let Some(encoded) = collect_data_payload(data_list) else {
        issues.push(FootprintValidationIssue::new(
            format!("Embedded file `{name}` has an invalid data payload"),
            list_span(data_list),
        ));
        return;
    };
    if encoded.is_empty() {
        return;
    }

    let Some(checksum_list) = find_child_list(file, "checksum") else {
        issues.push(FootprintValidationIssue::new(
            format!("Embedded file `{name}` with data is missing checksum"),
            file_span,
        ));
        return;
    };
    let checksum_span = list_span(checksum_list);
    let data_span = list_span(data_list);
    let Some(stored_checksum) = checksum_list.get(1).and_then(atom_text) else {
        issues.push(FootprintValidationIssue::new(
            format!("Embedded file `{name}` has an invalid checksum field"),
            checksum_span,
        ));
        return;
    };

    let compressed = match base64::engine::general_purpose::STANDARD.decode(encoded.as_bytes()) {
        Ok(bytes) => bytes,
        Err(err) => {
            issues.push(FootprintValidationIssue::new(
                format!("Embedded file `{name}` has invalid base64 data: {err}"),
                data_span,
            ));
            return;
        }
    };
    let decompressed = match zstd::decode_all(compressed.as_slice()) {
        Ok(bytes) => bytes,
        Err(err) => {
            issues.push(FootprintValidationIssue::new(
                format!("Embedded file `{name}` has invalid zstd data: {err}"),
                data_span,
            ));
            return;
        }
    };
    if !checksum_matches(&stored_checksum, &decompressed) {
        let mmh3 = embedded_file_checksum(&decompressed);
        let sha256 = sha256_hash(&decompressed);
        issues.push(FootprintValidationIssue::new(
            format!(
                "Embedded file `{name}` checksum mismatch: stored {stored_checksum}, expected {mmh3} or {sha256}",
            ),
            checksum_span,
        ));
    }
}

fn checksum_matches(stored: &str, data: &[u8]) -> bool {
    if stored.len() == 64 {
        stored.eq_ignore_ascii_case(&sha256_hash(data))
    } else {
        stored.eq_ignore_ascii_case(&embedded_file_checksum(data))
    }
}

fn sha256_hash(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Return the checksum KiCad currently writes for embedded file payloads.
///
/// This intentionally mirrors KiCad's streaming `MMH3_HASH`, including its
/// 4-byte tail padding and padded length finalization. That makes 12-15 byte
/// tails hash like an empty padded tail; odd, but required for byte-for-byte
/// compatibility with KiCad's embedded-file writer/loader.
pub fn embedded_file_checksum(data: &[u8]) -> String {
    const SEED: u64 = 0xABBA_2345;
    let mut h1 = SEED;
    let mut h2 = SEED;
    let mut len = 0usize;
    let mut block = [0u8; 16];

    let mut chunks = data.chunks_exact(16);
    for chunk in &mut chunks {
        block.copy_from_slice(chunk);
        hash_mmh3_block(&mut h1, &mut h2, &block);
        len += 16;
    }

    let tail = chunks.remainder();
    if !tail.is_empty() {
        block[..tail.len()].copy_from_slice(tail);
        let padding = 4 - (tail.len() + 4) % 4;
        block[tail.len()..tail.len() + padding].fill(0);
        len += tail.len() + padding;
    }

    hash_mmh3_tail(&mut h1, &mut h2, &block, len & 15);
    finish_mmh3(h1, h2, len as u64)
}

fn hash_mmh3_block(h1: &mut u64, h2: &mut u64, block: &[u8; 16]) {
    const C1: u64 = 0x87c3_7b91_1142_53d5;
    const C2: u64 = 0x4cf5_ad43_2745_937f;

    let mut k1 = u64::from_le_bytes(block[0..8].try_into().unwrap());
    let mut k2 = u64::from_le_bytes(block[8..16].try_into().unwrap());

    k1 = k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
    *h1 ^= k1;
    *h1 = h1
        .rotate_left(27)
        .wrapping_add(*h2)
        .wrapping_mul(5)
        .wrapping_add(0x52dc_e729);

    k2 = k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
    *h2 ^= k2;
    *h2 = h2
        .rotate_left(31)
        .wrapping_add(*h1)
        .wrapping_mul(5)
        .wrapping_add(0x3849_5ab5);
}

fn hash_mmh3_tail(h1: &mut u64, h2: &mut u64, tail: &[u8; 16], len: usize) {
    const C1: u64 = 0x87c3_7b91_1142_53d5;
    const C2: u64 = 0x4cf5_ad43_2745_937f;

    let mut k1 = 0u64;
    let mut k2 = 0u64;

    for (idx, byte) in tail.iter().copied().take(len.min(8)).enumerate() {
        k1 ^= (byte as u64) << (idx * 8);
    }
    for (idx, byte) in tail
        .iter()
        .copied()
        .skip(8)
        .take(len.saturating_sub(8))
        .enumerate()
    {
        k2 ^= (byte as u64) << (idx * 8);
    }

    if len > 8 {
        *h2 ^= k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
    }
    if len > 0 {
        *h1 ^= k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
    }
}

fn finish_mmh3(mut h1: u64, mut h2: u64, len: u64) -> String {
    h1 ^= len;
    h2 ^= len;

    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);

    h1 = fmix64(h1);
    h2 = fmix64(h2);

    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);

    format!("{h1:016X}{h2:016X}")
}

fn fmix64(mut value: u64) -> u64 {
    value ^= value >> 33;
    value = value.wrapping_mul(0xff51_afd7_ed55_8ccd);
    value ^= value >> 33;
    value = value.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    value ^= value >> 33;
    value
}

fn list_span(list: &[Sexpr]) -> Option<Span> {
    let start = list.first()?.span.start;
    let end = list.last()?.span.end;
    Some(Span::new(start, end))
}

fn collect_data_payload(data_list: &[Sexpr]) -> Option<String> {
    let raw = collect_child_atom_text(data_list, 1)?;
    Some(
        raw.trim()
            .trim_start_matches('|')
            .trim_end_matches('|')
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect(),
    )
}

fn collect_child_atom_text(list: &[Sexpr], start: usize) -> Option<String> {
    let mut text = String::new();
    for node in list.get(start..)? {
        text.push_str(&atom_text(node)?);
    }
    Some(text)
}

fn atom_text(node: &Sexpr) -> Option<String> {
    if let Some(raw) = &node.raw_atom {
        Some(raw.clone())
    } else {
        node.as_atom().map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn embedded_payload(bytes: &[u8]) -> (String, String) {
        let mut encoder = zstd::Encoder::new(Vec::new(), 17).unwrap();
        std::io::Write::write_all(&mut encoder, bytes).unwrap();
        let compressed = encoder.finish().unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(compressed);
        let checksum = hex::encode(Sha256::digest(bytes));
        (encoded, checksum)
    }

    fn footprint_with_data(data: &str, checksum: &str) -> String {
        format!(
            "(footprint \"Test\"\n  (embedded_files\n    (file\n      (name model.step)\n      (type model)\n      (data |{}|)\n      (checksum \"{}\")\n    )\n  )\n)",
            data, checksum
        )
    }

    #[test]
    fn valid_embedded_file_passes() {
        let (data, checksum) = embedded_payload(b"step model bytes");
        validate_footprint_source(&footprint_with_data(&data, &checksum)).unwrap();
    }

    #[test]
    fn multiline_embedded_file_passes() {
        let (data, checksum) = embedded_payload(b"step model bytes");
        let split = data.len() / 2;
        let data = format!("{}\n        {}", &data[..split], &data[split..]);
        validate_footprint_source(&footprint_with_data(&data, &checksum)).unwrap();
    }

    #[test]
    fn checksum_mismatch_fails() {
        let (data, _) = embedded_payload(b"step model bytes");
        let err = validate_footprint_source(&footprint_with_data(&data, "00")).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
        assert!(err.to_string().contains("stored 00"));
    }

    #[test]
    fn invalid_base64_fails() {
        let err = validate_footprint_source(&footprint_with_data("not base64", "00")).unwrap_err();
        assert!(err.to_string().contains("invalid base64 data"));
    }

    #[test]
    fn invalid_zstd_fails() {
        let data = base64::engine::general_purpose::STANDARD.encode(b"not zstd");
        let err = validate_footprint_source(&footprint_with_data(&data, "00")).unwrap_err();
        assert!(err.to_string().contains("invalid zstd data"));
    }

    #[test]
    fn footprint_without_embedded_files_passes() {
        validate_footprint_source("(footprint \"Test\")").unwrap();
    }

    #[test]
    fn legacy_module_root_passes() {
        validate_footprint_source("(module \"Test\")").unwrap();
    }

    #[test]
    fn embedded_file_without_data_passes() {
        validate_footprint_source(
            "(footprint \"Test\" (embedded_files (file (name model.step) (type model))))",
        )
        .unwrap();
    }

    #[test]
    fn embedded_file_with_empty_data_passes() {
        validate_footprint_source(
            "(footprint \"Test\" (embedded_files (file (name model.step) (type model) (data))))",
        )
        .unwrap();
    }

    #[test]
    fn current_mmh3_checksum_passes() {
        let (data, _) = embedded_payload(b"step model bytes");
        let checksum = embedded_file_checksum(b"step model bytes");
        validate_footprint_source(&footprint_with_data(&data, &checksum)).unwrap();
    }

    #[test]
    fn checksum_matches_kicad_padded_tail_output() {
        assert_eq!(
            embedded_file_checksum(b"STEP DATA HERE"),
            "0FC02384A29118F69FFB8F37551022E3"
        );
        assert_eq!(
            embedded_file_checksum(b"NEW STEP DATA"),
            "0FC02384A29118F69FFB8F37551022E3"
        );
    }

    #[test]
    fn checksum_accepts_kicad_padded_tail_output() {
        let payload = b"abcdefghijkl";
        let checksum = embedded_file_checksum(payload);
        assert_eq!(checksum, "0FC02384A29118F69FFB8F37551022E3");

        let (data, _) = embedded_payload(payload);
        validate_footprint_source(&footprint_with_data(&data, &checksum)).unwrap();
    }

    #[test]
    fn data_without_checksum_fails() {
        let (data, _) = embedded_payload(b"step model bytes");
        let footprint = format!(
            "(footprint \"Test\" (embedded_files (file (name model.step) (type model) (data |{}|))))",
            data
        );
        let err = validate_footprint_source(&footprint).unwrap_err();
        assert!(err.to_string().contains("with data is missing checksum"));
    }

    #[test]
    fn non_footprint_root_fails() {
        let err = validate_footprint_source("(kicad_pcb)").unwrap_err();
        assert!(
            err.to_string()
                .contains("root list must start with `footprint` or legacy `module`")
        );
    }
}
