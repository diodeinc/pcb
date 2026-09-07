use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) fn http_client() -> Result<Client> {
    let user_agent = format!("diode-pcb/{}", env!("CARGO_PKG_VERSION"));
    Client::builder()
        .user_agent(user_agent)
        .build()
        .context("Failed to build HTTP client")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadSource {
    Registry,
    KicadSymbols,
}

#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub source: DownloadSource,
    pub pct: Option<u8>,
    pub done: bool,
    pub error: Option<String>,
    pub is_update: bool,
}

pub(crate) struct ProgressReader<'a, R> {
    inner: R,
    downloaded: u64,
    total_size: Option<u64>,
    last_pct: u8,
    send_progress: &'a dyn Fn(Option<u8>, bool, Option<String>),
}

impl<'a, R> ProgressReader<'a, R> {
    pub(crate) fn new(
        inner: R,
        total_size: Option<u64>,
        send_progress: &'a dyn Fn(Option<u8>, bool, Option<String>),
    ) -> Self {
        Self {
            inner,
            downloaded: 0,
            total_size,
            last_pct: 0,
            send_progress,
        }
    }
}

impl<R: io::Read> io::Read for ProgressReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;
        self.downloaded += bytes_read as u64;

        if let Some(total) = self.total_size {
            let pct = (self.downloaded as f64 / total as f64 * 100.0) as u8;
            if pct != self.last_pct {
                (self.send_progress)(Some(pct), false, None);
                self.last_pct = pct;
            }
        }

        Ok(bytes_read)
    }
}

pub(crate) struct StderrProgressReader<R> {
    inner: R,
    downloaded: u64,
    total_size: Option<u64>,
    last_pct: u32,
    label: &'static str,
}

impl<R> StderrProgressReader<R> {
    pub(crate) fn new(inner: R, total_size: Option<u64>, label: &'static str) -> Self {
        Self {
            inner,
            downloaded: 0,
            total_size,
            last_pct: 0,
            label,
        }
    }
}

impl<R: io::Read> io::Read for StderrProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes_read = self.inner.read(buf)?;
        self.downloaded += bytes_read as u64;

        if let Some(total) = self.total_size {
            let pct = (self.downloaded as f64 / total as f64 * 100.0) as u32;
            if pct != self.last_pct {
                eprint!("\rDownloading {}... {}%", self.label, pct);
                self.last_pct = pct;
            }
        }

        Ok(bytes_read)
    }
}

fn version_file_path(db_path: &Path) -> PathBuf {
    db_path.with_extension("db.version")
}

pub(crate) fn load_local_version(db_path: &Path) -> Option<String> {
    let path = version_file_path(db_path);
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(crate) fn save_local_version(db_path: &Path, version: &str, label: &str) -> Result<()> {
    let path = version_file_path(db_path);
    AtomicFile::new(&path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(version.as_bytes())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to write local {label} version: {err}"))?;
    Ok(())
}

pub(crate) fn sha256_version_token(sha256: &str, label: &str) -> Result<String> {
    let sha256 = sha256.trim();
    if !sha256.is_empty() {
        return Ok(sha256.to_string());
    }

    anyhow::bail!("{label} metadata missing sha256")
}

pub(crate) fn ensure_parent_dir(dest_path: &Path, label: &str) -> Result<()> {
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {label} directory"))?;
    }
    Ok(())
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
}

impl<R: io::Read> io::Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }
}

pub(crate) fn write_decoded_index<R: io::Read>(
    dest_path: &Path,
    reader: R,
    label: &str,
    expected_sha256: &str,
) -> Result<()> {
    // API checksums cover the compressed zstd bytes, not the decoded SQLite file.
    let reader = HashingReader {
        inner: reader,
        hasher: Sha256::new(),
    };
    let mut decoder =
        zstd::stream::Decoder::new(reader).context("Failed to create zstd decoder")?;
    AtomicFile::new(dest_path, OverwriteBehavior::AllowOverwrite)
        .write(|file| {
            io::copy(&mut decoder, file).map_err(|err| {
                io::Error::new(
                    err.kind(),
                    format!("Failed to decompress and write {label}: {err}"),
                )
            })?;
            let actual = hex::encode(decoder.finish().into_inner().hasher.finalize());
            if !actual.eq_ignore_ascii_case(expected_sha256.trim()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{label} hash mismatch: expected {expected_sha256}, got {actual}"),
                ));
            }
            file.flush()
        })
        .with_context(|| format!("Failed to move downloaded {label} into place"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_compressed_hash_before_replacing_index() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("index.db");
        let payload = b"SQLite index bytes";
        let compressed = zstd::encode_all(payload.as_slice(), 0).unwrap();
        let compressed_hash = hex::encode(Sha256::digest(&compressed));
        let decoded_hash = hex::encode(Sha256::digest(payload));

        // A decoded-content checksum must fail, both on first download and update.
        for prior in [None, Some(b"cached index".as_slice())] {
            if let Some(prior) = prior {
                fs::write(&dest, prior).unwrap();
            }
            let err = write_decoded_index(&dest, compressed.as_slice(), "index", &decoded_hash)
                .unwrap_err();
            assert!(format!("{err:#}").contains("hash mismatch"));
            assert_eq!(fs::read(&dest).ok().as_deref(), prior);
        }

        write_decoded_index(&dest, compressed.as_slice(), "index", &compressed_hash).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), payload);
    }
}
