//! Portable, data-only DFM bundles. The manifest describes exact payload bytes;
//! archive metadata and compression framing are independent of the host machine.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

const MIB: u64 = 1024 * 1024;
const MAX_COMPRESSED_BYTES: u64 = 64 * MIB;
const MAX_ARCHIVE_BYTES: u64 = 256 * MIB;
const MAX_REPORT_BYTES: u64 = 128 * MIB;
const MAX_IPC_BYTES: u64 = 128 * MIB;
const MAX_PDK_BYTES: u64 = MIB;
const MAX_WAIVER_BYTES: u64 = 8 * MIB;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const TAR_BLOCK_BYTES: u64 = 512;

/// Preparation failures can still produce an incomplete report without sources
/// that could not be loaded. Complete reports supply the checked XML and PDK.
#[derive(Default)]
pub(super) struct Sources<'a> {
    pub ipc_xml: Option<&'a [u8]>,
    pub pdk: Option<&'a [u8]>,
    pub waivers: Option<&'a [u8]>,
}

#[derive(Serialize)]
struct Manifest {
    format: &'static str,
    schema_version: u32,
    report: &'static str,
    files: Vec<ManifestFile>,
}

#[derive(Serialize)]
struct ManifestFile {
    path: &'static str,
    media_type: &'static str,
    size_bytes: u64,
    sha256: String,
}

/// Replace `output` only after the entire bundle has been written successfully.
/// JSON validation and complete/incomplete report semantics belong to the caller.
pub(super) fn write(output: &Path, report: &[u8], sources: Sources<'_>) -> Result<()> {
    write_with_limit(output, report, sources, MAX_COMPRESSED_BYTES)
}

fn write_with_limit(
    output: &Path,
    report: &[u8],
    sources: Sources<'_>,
    compressed_limit: u64,
) -> Result<()> {
    let mut files = Vec::with_capacity(4);
    let mut inventory = Vec::with_capacity(4);
    for (path, media_type, bytes, limit) in [
        (
            "report.json",
            "application/json",
            Some(report),
            MAX_REPORT_BYTES,
        ),
        (
            "source/design.ipc2581.xml",
            "application/xml",
            sources.ipc_xml,
            MAX_IPC_BYTES,
        ),
        (
            "source/pdk.toml",
            "application/toml",
            sources.pdk,
            MAX_PDK_BYTES,
        ),
        (
            "source/waivers.toml",
            "application/toml",
            sources.waivers,
            MAX_WAIVER_BYTES,
        ),
    ] {
        if let Some(bytes) = bytes {
            ensure!(
                bytes.len() as u64 <= limit,
                "DFM bundle {path} exceeds the {limit} byte limit"
            );
            files.push((path, bytes));
            inventory.push(ManifestFile {
                path,
                media_type,
                size_bytes: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(bytes)),
            });
        }
    }
    let manifest = serde_json::to_vec_pretty(&Manifest {
        format: "pcb-dfm-report",
        schema_version: 1,
        report: "report.json",
        files: inventory,
    })?;
    ensure!(
        manifest.len() as u64 <= MAX_MANIFEST_BYTES,
        "DFM bundle manifest exceeds the {MAX_MANIFEST_BYTES} byte limit"
    );

    // Each member has a header and block-padded data; tar::Builder writes exactly
    // two additional zero records. Pledging this size lets readers reject a large
    // archive before allocating or decompressing its payload.
    let archive_bytes = 2 * TAR_BLOCK_BYTES
        + tar_member_bytes(manifest.len() as u64)
        + files
            .iter()
            .map(|(_, bytes)| tar_member_bytes(bytes.len() as u64))
            .sum::<u64>();
    ensure!(
        archive_bytes <= MAX_ARCHIVE_BYTES,
        "DFM bundle expanded archive exceeds the {MAX_ARCHIVE_BYTES} byte limit"
    );

    let parent = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("failed to create DFM bundle in {}", parent.display()))?;
    {
        let bounded = BoundedWriter {
            inner: temporary.as_file_mut(),
            remaining: compressed_limit,
        };
        let mut encoder = zstd::stream::write::Encoder::new(bounded, 3)?;
        encoder.include_checksum(true)?;
        encoder.include_contentsize(true)?;
        encoder.set_pledged_src_size(Some(archive_bytes))?;
        encoder.window_log(23)?; // Readers need at most an 8 MiB history window.

        let mut archive = tar::Builder::new(encoder);
        append(&mut archive, "manifest.json", &manifest)?;
        for (path, bytes) in files {
            append(&mut archive, path, bytes)?;
        }
        let encoder = archive
            .into_inner()
            .context("failed to finish DFM archive")?;
        encoder
            .finish()
            .context("failed to finish DFM bundle compression")?
            .flush()?;
    }
    temporary
        .as_file()
        .sync_all()
        .context("failed to flush DFM bundle to disk")?;
    temporary
        .persist(output)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace DFM bundle {}", output.display()))?;
    Ok(())
}

fn tar_member_bytes(size: u64) -> u64 {
    TAR_BLOCK_BYTES + size.div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES
}

fn append<W: Write>(archive: &mut tar::Builder<W>, path: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_ustar();
    header.set_path(path)?;
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_username("")?;
    header.set_groupname("")?;
    header.set_cksum();
    // The fixed short paths fit in ustar headers; append never emits long-name,
    // PAX, directory, symlink, or filesystem metadata entries.
    archive
        .append(&header, bytes)
        .with_context(|| format!("failed to write DFM bundle member {path}"))
}

struct BoundedWriter<W> {
    inner: W,
    remaining: u64,
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "DFM bundle exceeds the compressed byte limit",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const REPORT: &[u8] = br#"{"schema_version":1,"verdict":"incomplete"}"#;
    const XML: &[u8] = b"<?xml version=\"1.0\"?><IPC-2581/>\n";
    const PDK: &[u8] = b"[meta]\nname = \"Example\"\n";
    const WAIVERS: &[u8] = b"schema_version = 1\nwaivers = []\n";

    fn all_sources() -> Sources<'static> {
        Sources {
            ipc_xml: Some(XML),
            pdk: Some(PDK),
            waivers: Some(WAIVERS),
        }
    }

    #[test]
    fn manifest_inventory_and_ustar_metadata_describe_exact_payloads() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.tar.zst");
        write(&output, REPORT, all_sources()).unwrap();
        let compressed = std::fs::read(output).unwrap();
        let decoded = zstd::decode_all(compressed.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoded.as_slice());
        let mut files = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let header = entry.header();
            assert!(header.as_ustar().is_some());
            assert_eq!(header.entry_type(), tar::EntryType::Regular);
            assert_eq!(header.uid().unwrap(), 0);
            assert_eq!(header.gid().unwrap(), 0);
            assert_eq!(header.mtime().unwrap(), 0);
            assert_eq!(header.mode().unwrap(), 0o644);
            assert_eq!(header.username().unwrap(), Some(""));
            assert_eq!(header.groupname().unwrap(), Some(""));
            assert!(entry.link_name().unwrap().is_none());
            let path = entry.path().unwrap().into_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            files.push((path, bytes));
        }
        let expected = [
            ("report.json", "application/json", REPORT),
            ("source/design.ipc2581.xml", "application/xml", XML),
            ("source/pdk.toml", "application/toml", PDK),
            ("source/waivers.toml", "application/toml", WAIVERS),
        ];
        assert_eq!(files.len(), expected.len() + 1);
        assert_eq!(files[0].0, Path::new("manifest.json"));
        let manifest: serde_json::Value = serde_json::from_slice(&files[0].1).unwrap();
        assert_eq!(manifest["format"], "pcb-dfm-report");
        assert_eq!(manifest["schema_version"], 1);
        assert_eq!(manifest["report"], "report.json");
        assert_eq!(manifest["files"].as_array().unwrap().len(), expected.len());
        for (index, (path, media_type, bytes)) in expected.iter().enumerate() {
            assert_eq!(files[index + 1].0, Path::new(path));
            assert_eq!(files[index + 1].1, *bytes);
            assert_eq!(
                manifest["files"][index],
                serde_json::json!({
                    "path": path,
                    "media_type": media_type,
                    "size_bytes": bytes.len(),
                    "sha256": hex::encode(Sha256::digest(bytes)),
                })
            );
        }
        let end_records_start = decoded.len() - 1024;
        assert!(decoded[end_records_start..].iter().all(|byte| *byte == 0));
        assert_eq!(
            decoded.len() as u64,
            files
                .iter()
                .map(|(_, bytes)| 512 + (bytes.len() as u64).div_ceil(512) * 512)
                .sum::<u64>()
                + 1024
        );
    }

    #[test]
    fn incomplete_bundle_omits_unavailable_sources() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("incomplete.tar.zst");
        write(&output, REPORT, Sources::default()).unwrap();
        let compressed = std::fs::read(output).unwrap();
        let decoded = zstd::decode_all(compressed.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoded.as_slice());
        let mut entries = archive.entries().unwrap();
        let mut entry = entries.next().unwrap().unwrap();
        let manifest: serde_json::Value = serde_json::from_reader(&mut entry).unwrap();
        assert_eq!(manifest["files"].as_array().unwrap().len(), 1);
        assert_eq!(manifest["files"][0]["path"], "report.json");
        assert_eq!(
            entries.next().unwrap().unwrap().path().unwrap(),
            Path::new("report.json")
        );
        assert!(entries.next().is_none());
    }

    #[test]
    fn identical_payloads_produce_identical_bundles_and_replace_existing_output() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.tar.zst");
        let second = directory.path().join("second.tar.zst");
        std::fs::write(&first, b"previous artifact").unwrap();
        write(&first, REPORT, all_sources()).unwrap();
        write(&second, REPORT, all_sources()).unwrap();
        assert_eq!(
            std::fs::read(first).unwrap(),
            std::fs::read(second).unwrap()
        );
    }

    #[test]
    fn zstd_frame_has_bounded_window_declared_size_and_verified_checksum() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("large.tar.zst");
        // Exceed the maximum window to exercise a non-single-segment frame.
        let mut report = vec![b' '; 9 * MIB as usize];
        report[..2].copy_from_slice(b"{}");
        write(&output, &report, Sources::default()).unwrap();
        let mut compressed = std::fs::read(output).unwrap();
        assert_eq!(&compressed[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
        assert_eq!(compressed[4] & 0b0000_0100, 0b0000_0100); // Checksum.
        assert_eq!(compressed[4] & 0b0000_0011, 0); // No dictionary ID.
        assert_eq!(compressed[4] & 0b0010_0000, 0); // Separate window size.
        let window = compressed[5];
        let base = 1_u64 << (10 + (window >> 3));
        let window_bytes = base + base / 8 * u64::from(window & 7);
        assert!(window_bytes <= 8 * MIB);
        assert_eq!(
            zstd::zstd_safe::find_frame_compressed_size(&compressed).unwrap(),
            compressed.len(),
            "the archive must contain exactly one frame and no trailing bytes"
        );
        let decoded = zstd::decode_all(compressed.as_slice()).unwrap();
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&compressed).unwrap(),
            Some(decoded.len() as u64)
        );
        *compressed.last_mut().unwrap() ^= 1;
        assert!(zstd::decode_all(compressed.as_slice()).is_err());
    }

    #[test]
    fn compression_write_failure_preserves_existing_output_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.tar.zst");
        std::fs::write(&output, b"previous artifact").unwrap();
        let error = write_with_limit(&output, REPORT, all_sources(), 16).unwrap_err();
        assert!(format!("{error:#}").contains("compressed byte limit"));
        assert_eq!(std::fs::read(output).unwrap(), b"previous artifact");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn oversize_member_is_rejected_before_replacing_output() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.tar.zst");
        std::fs::write(&output, b"previous artifact").unwrap();
        let pdk = vec![b' '; MAX_PDK_BYTES as usize + 1];
        let error = write(
            &output,
            REPORT,
            Sources {
                pdk: Some(&pdk),
                ..Sources::default()
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("source/pdk.toml exceeds"));
        assert_eq!(std::fs::read(output).unwrap(), b"previous artifact");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn persistence_failure_preserves_destination_and_cleans_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("report.tar.zst");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("sentinel"), b"untouched").unwrap();
        let error = write(&output, REPORT, all_sources()).unwrap_err();
        assert!(format!("{error:#}").contains("failed to replace DFM bundle"));
        assert_eq!(
            std::fs::read(output.join("sentinel")).unwrap(),
            b"untouched"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
