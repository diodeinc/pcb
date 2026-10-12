//! An optional, persistent cache of tessellated models on local disk.
//!
//! It is off unless `PCB_GLTF_MODEL_CACHE_DIR` names a directory (or an
//! embedder passes a [`ModelCache`] to [`export_with_cache`]). An entry
//! holds what tessellating and simplifying one decoded STEP payload gave,
//! not where the model is placed or what it is called, so a payload shared
//! by two boards, or two revisions of one board, is tessellated once.
//!
//! An entry is keyed by the SHA-256 of a salt, the effective simplification
//! tolerance (which carries the model's scale) and the decoded STEP bytes.
//! The salt is the entry format, this crate's version and the sources of
//! `mesh.rs` and `models.rs`. Bump [`FORMAT`] when a tessellation
//! dependency (foxtrot, meshopt-rs) changes what it produces.
//!
//! The cache can only make an export faster: an entry that is missing,
//! unreadable, truncated or fails its checksum is recomputed and rewritten,
//! and a directory that cannot be written is only read. Entries are renamed
//! into place from a unique temporary file, so a reader sees a whole entry
//! or none. Nothing is added to the export's report, so the output and its
//! diagnostics are the same with the cache on or off. Nothing evicts
//! entries; the directory belongs to whoever sets it.
//!
//! [`export_with_cache`]: crate::export_with_cache

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::mesh::Primitive;

/// The environment variable naming the cache directory.
const ENV: &str = "PCB_GLTF_MODEL_CACHE_DIR";

/// Bump when the entry encoding, or what a tessellation dependency
/// produces, changes.
const FORMAT: &str = "pcb-gltf model cache 3";

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
}

/// A directory of tessellated models, shared by any number of threads and
/// processes.
pub struct ModelCache {
    dir: PathBuf,
    salt: Key,
    stats: Mutex<CacheStats>,
}

impl ModelCache {
    /// The cache `PCB_GLTF_MODEL_CACHE_DIR` names; `None` when it is unset
    /// or the directory cannot be created.
    pub fn from_env() -> Option<Self> {
        let dir = std::env::var_os(ENV).filter(|d| !d.is_empty())?;
        Self::open(dir)
            .inspect_err(|err| log::warn!("model cache disabled: {err}"))
            .ok()
    }

    /// Use `dir`, creating it if need be.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mut salt = Sha256::new();
        for part in [
            FORMAT,
            env!("CARGO_PKG_VERSION"),
            include_str!("mesh.rs"),
            include_str!("models.rs"),
        ] {
            salt.update((part.len() as u64).to_le_bytes());
            salt.update(part);
        }
        Ok(Self {
            dir,
            salt: salt.finalize().into(),
            stats: Mutex::default(),
        })
    }

    pub fn stats(&self) -> CacheStats {
        *self.stats.lock().unwrap()
    }

    pub(crate) fn key(&self, payload: &[u8], simplify_error: f32) -> Key {
        let mut hasher = Sha256::new();
        hasher.update(self.salt);
        hasher.update(simplify_error.to_bits().to_le_bytes());
        hasher.update(payload);
        hasher.finalize().into()
    }

    fn path(&self, key: &Key) -> PathBuf {
        let name: String = key.iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(name + ".mesh")
    }

    pub(crate) fn get(&self, key: &Key) -> Option<Tessellation> {
        let found = fs::read(self.path(key)).ok().and_then(|data| decode(&data));
        let mut stats = self.stats.lock().unwrap();
        *if found.is_some() {
            &mut stats.hits
        } else {
            &mut stats.misses
        } += 1;
        found
    }

    pub(crate) fn put(&self, key: &Key, tessellation: &Tessellation) {
        // A uniquely named temporary file, so writers in other processes,
        // or containers sharing the directory, never share one.
        let written = tempfile::Builder::new()
            .prefix(".entry-")
            .tempfile_in(&self.dir)
            .and_then(|mut tmp| {
                tmp.write_all(&encode(tessellation))?;
                tmp.persist(self.path(key)).map_err(|e| e.error)
            });
        if let Err(err) = written {
            log::warn!("model cache: cannot write to {}: {err}", self.dir.display());
        }
    }

    pub(crate) fn log_summary(&self) {
        log::debug!("model cache {}: {:?}", self.dir.display(), self.stats());
    }
}

/// `sha256(body) body`, with the body in little-endian fixed-width fields.
fn encode(t: &Tessellation) -> Vec<u8> {
    fn len(body: &mut Vec<u8>, n: usize) {
        body.extend((n as u64).to_le_bytes());
    }
    fn floats<'a>(body: &mut Vec<u8>, values: impl IntoIterator<Item = &'a f32>) {
        values
            .into_iter()
            .for_each(|v| body.extend(v.to_bits().to_le_bytes()));
    }
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
            len(&mut body, reason.len());
            body.extend(reason.as_bytes());
        }
        Ok(primitives) => {
            body.push(1);
            len(&mut body, primitives.len());
            for (color, p) in primitives {
                floats(&mut body, color);
                len(&mut body, p.positions.len());
                floats(&mut body, p.positions.iter().flatten());
                len(&mut body, p.normals.len());
                floats(&mut body, p.normals.iter().flatten());
                len(&mut body, p.indices.len());
                p.indices.iter().for_each(|i| body.extend(i.to_le_bytes()));
            }
        }
    }
    [Sha256::digest(&body).as_slice(), &body].concat()
}

/// The tessellation `data` encodes, if it is whole.
fn decode(data: &[u8]) -> Option<Tessellation> {
    let (sum, body) = data.split_at_checked(32)?;
    if Sha256::digest(body).as_slice() != sum {
        return None;
    }
    let mut r = Reader(body);
    let face_failures = match r.u8()? {
        0 => None,
        1 => Some((r.u64()?, r.u64()?)),
        _ => return None,
    };
    let primitives = match r.u8()? {
        0 => {
            let len = r.count(1)?;
            Err(String::from_utf8(r.bytes(len)?.to_vec()).ok()?)
        }
        1 => {
            let n = r.count(16)?;
            let mut primitives = Vec::new();
            for _ in 0..n {
                let color = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
                let positions = r.vec3s()?;
                let normals = r.vec3s()?;
                let n = r.count(4)?;
                let indices = (0..n).map(|_| r.u32()).collect::<Option<_>>()?;
                let primitive = Primitive {
                    positions,
                    normals,
                    indices,
                };
                primitives.push((color, primitive));
            }
            Ok(primitives)
        }
        _ => return None,
    };
    r.0.is_empty().then_some(Tessellation {
        face_failures,
        primitives,
    })
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk()?;
        self.0 = rest;
        Some(*head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[b]| b)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }
    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }
    fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.0.split_at_checked(n)?;
        self.0 = rest;
        Some(head)
    }
    /// A length of items of at least `size` bytes each.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_and_damage_is_refused() {
        let primitive = Primitive {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, -0.0], [0.0, 1.5, 2.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            indices: vec![0, 1, 2],
        };
        let t = Tessellation {
            face_failures: Some((2, 9)),
            primitives: Ok(vec![([0.1, 0.2, 0.3, 1.0], primitive)]),
        };
        let data = encode(&t);
        assert_eq!(decode(&data), Some(t));
        for len in 0..data.len() {
            assert!(decode(&data[..len]).is_none(), "truncated to {len}");
        }
        for i in 0..data.len() {
            let mut bad = data.clone();
            bad[i] ^= 0x40;
            assert!(decode(&bad).is_none(), "byte {i} flipped");
        }
    }
}
