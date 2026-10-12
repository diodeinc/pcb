//! An optional, persistent cache of tessellated models on local disk.
//!
//! The cache is off unless `PCB_GLTF_MODEL_CACHE_DIR` names a directory (or
//! an embedder passes a [`ModelCache`] to [`export_with_cache`]). It holds
//! what tessellating and simplifying one decoded STEP payload gave, and
//! nothing about where the model is placed or what it is called: the name
//! and the warnings that mention it are rebuilt from whichever model hits,
//! so a payload shared by two boards, or two revisions of one board, is
//! tessellated once.
//!
//! An entry is keyed by the SHA-256 of
//!
//! - a salt: the entry format, the CPU architecture, the sources of
//!   `mesh.rs` and `models.rs`, and the locked
//!   versions of the crates they tessellate with (see `build.rs`);
//! - the effective simplification tolerance, bit for bit, which carries the
//!   model's scale;
//! - the decoded STEP bytes.
//!
//! The cache can only make an export faster. Every failure to read or
//! write it is a miss: an entry that is unreadable, truncated, fails its
//! checksum, belongs to another key or does not decode is recomputed and
//! rewritten. Entries are written to a temporary file in their directory
//! and renamed into place, so a reader sees a whole entry or none, and
//! concurrent writers of one key (which hold identical bytes) cannot
//! interleave. Problems are logged, never added to the export's report, so
//! the output and its diagnostics are the same with the cache on or off.
//!
//! Nothing evicts entries; the directory belongs to whoever sets it.
//!
//! [`export_with_cache`]: crate::export_with_cache

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use sha2::{Digest, Sha256};

use crate::mesh::Primitive;

/// The environment variable naming the cache directory.
pub const ENV: &str = "PCB_GLTF_MODEL_CACHE_DIR";

/// Bump when the entry encoding changes.
const FORMAT: &str = "pcb-gltf model cache entry 1";

const MAGIC: &[u8; 8] = b"PCBGMC01";
const HEADER: usize = MAGIC.len() + 32 + 32;

/// An entry larger than this is not read, whatever it claims to be.
const MAX_ENTRY: u64 = 1 << 30;

/// Locked tessellation dependencies, from `build.rs`; empty if unknown.
const DEPENDENCIES: &str = env!("PCB_GLTF_MODEL_CACHE_DEPS");

/// What tessellating one payload gave, independent of the model's name.
#[derive(Debug, PartialEq)]
pub(crate) struct Tessellation {
    /// How many faces could not be tessellated, of how many, if any failed.
    pub(crate) face_failures: Option<(u64, u64)>,
    /// Simplified, optimized primitives by colour, none when the model has
    /// no faces, or why the payload could not be tessellated.
    pub(crate) primitives: Result<Vec<([f32; 4], Primitive)>, String>,
}

pub(crate) type Key = [u8; 32];

/// What a [`ModelCache`] has done so far.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Entries found damaged, a subset of the misses.
    pub corrupt: u64,
    pub stored: u64,
    pub write_failures: u64,
    pub bytes_read: u64,
    pub bytes_written: u64,
}

/// A directory of tessellated models, shared by any number of threads and
/// processes.
pub struct ModelCache {
    dir: PathBuf,
    salt: Key,
    writable: AtomicBool,
    hits: AtomicU64,
    misses: AtomicU64,
    corrupt: AtomicU64,
    stored: AtomicU64,
    write_failures: AtomicU64,
    bytes_read: AtomicU64,
    bytes_written: AtomicU64,
}

impl ModelCache {
    /// The cache `PCB_GLTF_MODEL_CACHE_DIR` names; `None` when it is unset
    /// or the directory cannot be used.
    pub fn from_env() -> Option<Self> {
        let dir = std::env::var_os(ENV).filter(|d| !d.is_empty())?;
        match Self::open(&dir) {
            Ok(cache) => Some(cache),
            Err(err) => {
                log::warn!("model cache disabled: {err}");
                None
            }
        }
    }

    /// Use `dir`, creating it if need be. Fails when it cannot be created
    /// or the build does not know what its entries would depend on.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        if DEPENDENCIES.is_empty() {
            return Err(io::Error::other(
                "this build cannot fingerprint its tessellation dependencies",
            ));
        }
        fs::create_dir_all(&dir).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("cannot create {}: {err}", dir.display()),
            )
        })?;
        let mut salt = Sha256::new();
        for part in [
            FORMAT,
            std::env::consts::ARCH,
            DEPENDENCIES,
            include_str!("mesh.rs"),
            include_str!("models.rs"),
        ] {
            salt.update((part.len() as u64).to_le_bytes());
            salt.update(part);
        }
        Ok(Self::with_salt(dir, salt.finalize().into()))
    }

    fn with_salt(dir: PathBuf, salt: Key) -> Self {
        Self {
            dir,
            salt,
            writable: AtomicBool::new(true),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            corrupt: AtomicU64::new(0),
            stored: AtomicU64::new(0),
            write_failures: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            bytes_written: AtomicU64::new(0),
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits.load(Relaxed),
            misses: self.misses.load(Relaxed),
            corrupt: self.corrupt.load(Relaxed),
            stored: self.stored.load(Relaxed),
            write_failures: self.write_failures.load(Relaxed),
            bytes_read: self.bytes_read.load(Relaxed),
            bytes_written: self.bytes_written.load(Relaxed),
        }
    }

    pub(crate) fn key(&self, payload: &[u8], simplify_error: f32) -> Key {
        let mut hasher = Sha256::new();
        hasher.update(self.salt);
        hasher.update(simplify_error.to_bits().to_le_bytes());
        hasher.update((payload.len() as u64).to_le_bytes());
        hasher.update(payload);
        hasher.finalize().into()
    }

    fn path(&self, key: &Key) -> PathBuf {
        let name = hex(key);
        self.dir.join(&name[..2]).join(format!("{name}.mesh"))
    }

    pub(crate) fn get(&self, key: &Key) -> Option<Tessellation> {
        let path = self.path(key);
        match self.read(&path, key) {
            Ok(Some((tessellation, size))) => {
                self.hits.fetch_add(1, Relaxed);
                self.bytes_read.fetch_add(size, Relaxed);
                Some(tessellation)
            }
            Ok(None) => {
                self.misses.fetch_add(1, Relaxed);
                None
            }
            Err(reason) => {
                self.corrupt.fetch_add(1, Relaxed);
                self.misses.fetch_add(1, Relaxed);
                log::warn!("model cache: discarding {}: {reason}", path.display());
                None
            }
        }
    }

    /// The entry at `path`, if there is one: `Err` says why it is no good.
    fn read(&self, path: &Path, key: &Key) -> Result<Option<(Tessellation, u64)>, String> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(format!("unreadable: {err}")),
        };
        let size = file
            .metadata()
            .map_err(|e| format!("unreadable: {e}"))?
            .len();
        if size > MAX_ENTRY {
            return Err(format!("{size} bytes is too large"));
        }
        let mut data = Vec::with_capacity(size as usize);
        io::Read::read_to_end(&mut &file, &mut data).map_err(|e| format!("unreadable: {e}"))?;
        let tessellation = decode(&data, key)?;
        Ok(Some((tessellation, data.len() as u64)))
    }

    pub(crate) fn put(&self, key: &Key, tessellation: &Tessellation) {
        if !self.writable.load(Relaxed) {
            return;
        }
        match self.write(key, tessellation) {
            Ok(bytes) => {
                self.stored.fetch_add(1, Relaxed);
                self.bytes_written.fetch_add(bytes, Relaxed);
            }
            Err(err) => {
                self.write_failures.fetch_add(1, Relaxed);
                // Most likely a read-only or full disk: do not keep trying.
                if self.writable.swap(false, Relaxed) {
                    log::warn!(
                        "model cache: cannot write to {}, reading only: {err}",
                        self.dir.display()
                    );
                }
            }
        }
    }

    fn write(&self, key: &Key, tessellation: &Tessellation) -> io::Result<u64> {
        let path = self.path(key);
        let shard = path.parent().expect("entries are in a shard directory");
        fs::create_dir_all(shard)?;
        let data = encode(tessellation, key);
        // A uniquely named temporary file, so writers in other processes,
        // or other containers sharing the directory, never share one.
        let mut tmp = tempfile::Builder::new()
            .prefix(".entry-")
            .suffix(".tmp")
            .tempfile_in(shard)?;
        tmp.write_all(&data)?;
        tmp.persist(&path).map_err(|e| e.error)?;
        Ok(data.len() as u64)
    }

    pub(crate) fn log_summary(&self) {
        let s = self.stats();
        log::debug!(
            "model cache {}: {} hit(s), {} miss(es), {} corrupt, {} stored, {} write failure(s); \
             {} bytes read, {} bytes written",
            self.dir.display(),
            s.hits,
            s.misses,
            s.corrupt,
            s.stored,
            s.write_failures,
            s.bytes_read,
            s.bytes_written,
        );
    }
}

/// `MAGIC key sha256(body) body`: the key makes an entry that was copied
/// or renamed to the wrong name a miss.
fn encode(t: &Tessellation, key: &Key) -> Vec<u8> {
    let mut body = Vec::new();
    match t.face_failures {
        None => body.push(0),
        Some((failures, faces)) => {
            body.push(1);
            body.extend(failures.to_le_bytes());
            body.extend(faces.to_le_bytes());
        }
    }
    match &t.primitives {
        Err(reason) => {
            body.push(0);
            body.extend((reason.len() as u64).to_le_bytes());
            body.extend(reason.as_bytes());
        }
        Ok(primitives) => {
            body.push(1);
            body.extend((primitives.len() as u64).to_le_bytes());
            for (color, p) in primitives {
                color
                    .iter()
                    .for_each(|c| body.extend(c.to_bits().to_le_bytes()));
                body.extend((p.positions.len() as u64).to_le_bytes());
                p.positions
                    .iter()
                    .flatten()
                    .for_each(|v| body.extend(v.to_bits().to_le_bytes()));
                body.extend((p.normals.len() as u64).to_le_bytes());
                p.normals
                    .iter()
                    .flatten()
                    .for_each(|v| body.extend(v.to_bits().to_le_bytes()));
                body.extend((p.indices.len() as u64).to_le_bytes());
                p.indices.iter().for_each(|i| body.extend(i.to_le_bytes()));
            }
        }
    }
    let mut data = Vec::with_capacity(HEADER + body.len());
    data.extend_from_slice(MAGIC);
    data.extend_from_slice(key);
    data.extend_from_slice(&Sha256::digest(&body));
    data.extend_from_slice(&body);
    data
}

fn decode(data: &[u8], key: &Key) -> Result<Tessellation, String> {
    if data.len() < HEADER || &data[..8] != MAGIC {
        return Err("bad header".into());
    }
    if data[8..40] != key[..] {
        return Err("entry is for another key".into());
    }
    let body = &data[HEADER..];
    if Sha256::digest(body).as_slice() != &data[40..HEADER] {
        return Err("checksum mismatch".into());
    }
    let mut r = Reader(body);
    let t = (|| {
        let face_failures = match r.u8()? {
            0 => None,
            1 => Some((r.u64()?, r.u64()?)),
            _ => return None,
        };
        let primitives = match r.u8()? {
            0 => {
                let len = r.count(1)?;
                Err(String::from_utf8(r.take(len)?.to_vec()).ok()?)
            }
            1 => {
                // Each primitive takes at least its colour and counts.
                let n = r.count(16 + 24)?;
                let mut primitives = Vec::with_capacity(n);
                for _ in 0..n {
                    let color = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
                    let positions = r.vec3s()?;
                    let normals = r.vec3s()?;
                    let len = r.count(4)?;
                    let indices = (0..len).map(|_| r.u32()).collect::<Option<Vec<_>>>()?;
                    primitives.push((
                        color,
                        Primitive {
                            positions,
                            normals,
                            indices,
                        },
                    ));
                }
                Ok(primitives)
            }
            _ => return None,
        };
        r.0.is_empty().then_some(Tessellation {
            face_failures,
            primitives,
        })
    })()
    .ok_or("does not decode")?;
    match &t.primitives {
        Ok(primitives) if !primitives.iter().all(|(_, p)| p.is_consistent()) => {
            Err("inconsistent mesh".into())
        }
        _ => Ok(t),
    }
}

impl Primitive {
    /// Whether the indices make triangles of the vertices.
    fn is_consistent(&self) -> bool {
        let n = self.positions.len();
        self.normals.len() == n
            && !self.indices.is_empty()
            && self.indices.len().is_multiple_of(3)
            && self.indices.iter().all(|&i| (i as usize) < n)
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }
    /// A length of items of `size` bytes at least, that the rest of the
    /// data could hold, so a damaged length cannot ask for a huge buffer.
    fn count(&mut self, size: usize) -> Option<usize> {
        let n = usize::try_from(self.u64()?).ok()?;
        (n.checked_mul(size)? <= self.0.len()).then_some(n)
    }
    fn vec3s(&mut self) -> Option<Vec<[f32; 3]>> {
        let n = self.count(12)?;
        (0..n)
            .map(|_| Some([self.f32()?, self.f32()?, self.f32()?]))
            .collect()
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Tessellation {
        let primitive = Primitive {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, -0.0],
                [0.0, 1.5, f32::MIN_POSITIVE],
            ],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2],
        };
        Tessellation {
            face_failures: Some((2, 9)),
            primitives: Ok(vec![([0.1, 0.2, 0.3, 1.0], primitive)]),
        }
    }

    fn cache(dir: &Path) -> ModelCache {
        ModelCache::with_salt(dir.to_path_buf(), [7; 32])
    }

    #[test]
    fn entries_round_trip_exactly() {
        let key = [3; 32];
        for t in [
            sample(),
            Tessellation {
                face_failures: None,
                primitives: Ok(Vec::new()),
            },
            Tessellation {
                face_failures: Some((1, 1)),
                primitives: Err("nothing".into()),
            },
        ] {
            assert_eq!(decode(&encode(&t, &key), &key).unwrap(), t);
        }
    }

    #[test]
    fn damaged_entries_are_refused() {
        let key = [3; 32];
        let good = encode(&sample(), &key);
        for len in 0..good.len() {
            assert!(decode(&good[..len], &key).is_err(), "truncated to {len}");
        }
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0x40;
            assert!(decode(&bad, &key).is_err(), "byte {i} flipped");
        }
        assert!(decode(&good, &[4; 32]).is_err());
        let mut long = good.clone();
        long.push(0);
        assert!(decode(&long, &key).is_err());
    }

    #[test]
    fn a_checksummed_but_nonsense_body_is_refused() {
        // A body whose counts overrun it, or whose indices point outside
        // the vertices, with a correct checksum.
        let key = [3; 32];
        let seal = |body: &[u8]| {
            let mut data = [&MAGIC[..], &key, &Sha256::digest(body)].concat();
            data.extend_from_slice(body);
            data
        };
        let mut huge = vec![0, 1];
        huge.extend(u64::MAX.to_le_bytes());
        assert!(decode(&seal(&huge), &key).is_err());

        let mut t = sample();
        t.primitives.as_mut().unwrap()[0].1.indices[2] = 3;
        let data = encode(&t, &key);
        assert_eq!(decode(&data, &key).unwrap_err(), "inconsistent mesh");
    }

    #[test]
    fn keys_depend_on_payload_tolerance_and_salt() {
        let dir = tempfile::tempdir().unwrap();
        let a = cache(dir.path());
        let k = a.key(b"step", 0.01);
        assert_eq!(k, a.key(b"step", 0.01));
        assert_ne!(k, a.key(b"step!", 0.01));
        assert_ne!(k, a.key(b"step", 0.005));
        assert_ne!(k, a.key(b"step", 0.01 + f32::EPSILON));
        let b = ModelCache::with_salt(dir.path().to_path_buf(), [8; 32]);
        assert_ne!(k, b.key(b"step", 0.01));
    }

    #[test]
    fn put_then_get_and_recover_from_damage() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let key = cache.key(b"x", 0.01);
        assert!(cache.get(&key).is_none());
        cache.put(&key, &sample());
        assert_eq!(cache.get(&key).unwrap(), sample());

        // Truncate it: a miss, counted as corrupt, then rewritten.
        let path = cache.path(&key);
        let len = fs::metadata(&path).unwrap().len();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len / 2)
            .unwrap();
        assert!(cache.get(&key).is_none());
        cache.put(&key, &sample());
        assert_eq!(cache.get(&key).unwrap(), sample());
        let s = cache.stats();
        assert_eq!((s.hits, s.misses, s.corrupt, s.stored), (2, 2, 1, 2));
        // No temporary files are left behind.
        let leftovers = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn unusable_storage_is_only_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        // The cache directory is a file.
        let file = dir.path().join("file");
        fs::write(&file, b"x").unwrap();
        assert!(ModelCache::open(&file).is_err());
        assert!(ModelCache::open(file.join("sub")).is_err());

        // A shard that cannot be written, or an entry that is a directory.
        let cache = cache(dir.path());
        let key = cache.key(b"y", 0.01);
        fs::create_dir_all(cache.path(&key)).unwrap();
        assert!(cache.get(&key).is_none());
        cache.put(&key, &sample());
        assert_eq!(cache.stats().write_failures, 1);
        // After a failed write, no more are attempted.
        cache.put(&cache.key(b"z", 0.01), &sample());
        assert_eq!(cache.stats().write_failures, 1);
    }

    #[test]
    fn concurrent_writers_and_readers_agree() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path());
        let keys: Vec<Key> = (0..4u8).map(|i| cache.key(&[i], 0.01)).collect();
        std::thread::scope(|scope| {
            for t in 0..8 {
                let (cache, keys) = (&cache, &keys);
                scope.spawn(move || {
                    for round in 0..50 {
                        let key = &keys[(t + round) % keys.len()];
                        match cache.get(key) {
                            Some(found) => assert_eq!(found, sample()),
                            None => cache.put(key, &sample()),
                        }
                    }
                });
            }
        });
        assert_eq!(cache.stats().corrupt, 0);
        assert_eq!(cache.stats().write_failures, 0);
        for key in &keys {
            assert_eq!(cache.get(key).unwrap(), sample());
        }
    }
}
