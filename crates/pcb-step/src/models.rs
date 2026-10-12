//! Model loading. A board is untrusted: files on disk are read only under
//! directories the caller allows in a [`ModelResolver`] (none by default),
//! with variables from the resolver, never the environment. A path that
//! leaves every root, by `..` or symlink, is absent like a missing file.

use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFormat {
    Step,
    Wrl,
}

impl ModelFormat {
    pub fn of_path(path: &str) -> Option<Self> {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".step") || lower.ends_with(".stp") {
            Some(ModelFormat::Step)
        } else if lower.ends_with(".wrl") {
            Some(ModelFormat::Wrl)
        } else {
            None
        }
    }
}

pub struct LoadedModel {
    pub bytes: Vec<u8>,
    pub format: ModelFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    Absent(String),
    Unsupported(String),
    Failed(String),
    Empty,
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::Absent(why) | ModelError::Unsupported(why) | ModelError::Failed(why) => {
                f.write_str(why)
            }
            ModelError::Empty => f.write_str("the model has no solid geometry"),
        }
    }
}

pub(crate) fn unsupported_format() -> ModelError {
    ModelError::Unsupported("only .step, .stp and .wrl models are read".to_owned())
}

pub const MAX_EXTERNAL_BYTES: u64 = 256 << 20;

#[derive(Debug, Clone, Default)]
pub struct ModelResolver {
    roots: Vec<PathBuf>,
    variables: Vec<(String, PathBuf)>,
}

impl ModelResolver {
    /// Relative model paths are tried under each root in order.
    pub fn allow_root(mut self, dir: &Path) -> std::io::Result<Self> {
        let dir = dir.canonicalize()?;
        if !self.roots.contains(&dir) {
            self.roots.push(dir);
        }
        Ok(self)
    }

    pub fn define(mut self, name: &str, dir: &Path) -> std::io::Result<Self> {
        self = self.allow_root(dir)?;
        self.variables.retain(|(n, _)| n != name);
        // Not canonical: `\\?\` paths take neither `/` nor `..`.
        self.variables
            .push((name.to_owned(), std::path::absolute(dir)?));
        Ok(self)
    }

    /// `Err` carries the first undefined name.
    fn expand(&self, path: &str) -> Result<String, String> {
        let mut out = String::with_capacity(path.len());
        let mut rest = path;
        while let Some(at) = rest.find('$') {
            out.push_str(&rest[..at]);
            let tail = &rest[at + 1..];
            let close = match tail.chars().next() {
                Some('{') => '}',
                Some('(') => ')',
                _ => {
                    out.push('$');
                    rest = tail;
                    continue;
                }
            };
            let Some(end) = tail.find(close) else {
                out.push('$');
                rest = tail;
                continue;
            };
            let name = &tail[1..end];
            let Some((_, value)) = self.variables.iter().find(|(n, _)| n == name) else {
                return Err(name.to_owned());
            };
            out.push_str(&value.to_string_lossy());
            rest = &tail[end + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    pub fn load(&self, reference: &str) -> Result<LoadedModel, ModelError> {
        let expanded = self
            .expand(reference)
            .map_err(|name| ModelError::Absent(format!("path variable {name} is not defined")))?;
        // A canonical Windows root is `\\?\C:\...`, which takes no `/`.
        #[cfg(windows)]
        let expanded = expanded.replace('/', "\\");
        let format = ModelFormat::of_path(&expanded).ok_or_else(unsupported_format)?;
        let path = Path::new(&expanded);
        let candidates: Vec<PathBuf> = if path.is_absolute() {
            vec![path.to_path_buf()]
        } else {
            self.roots.iter().map(|root| root.join(path)).collect()
        };
        for candidate in candidates {
            let Ok(real) = candidate.canonicalize() else {
                continue;
            };
            if !self.roots.iter().any(|root| real.starts_with(root)) {
                continue;
            }
            let failed = |why: String| ModelError::Failed(format!("{}: {why}", real.display()));
            let meta = std::fs::metadata(&real).map_err(|e| failed(e.to_string()))?;
            if !meta.is_file() {
                return Err(failed("not a regular file".to_owned()));
            }
            let too_large = || failed(format!("over the {MAX_EXTERNAL_BYTES} byte limit"));
            if meta.len() > MAX_EXTERNAL_BYTES {
                return Err(too_large());
            }
            let mut bytes = Vec::new();
            std::fs::File::open(&real)
                .and_then(|f| f.take(MAX_EXTERNAL_BYTES + 1).read_to_end(&mut bytes))
                .map_err(|e| failed(e.to_string()))?;
            if bytes.len() as u64 > MAX_EXTERNAL_BYTES {
                return Err(too_large());
            }
            return Ok(LoadedModel { bytes, format });
        }
        Err(ModelError::Absent(
            "no such file under the allowed model directories".to_owned(),
        ))
    }
}
