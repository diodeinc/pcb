use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

pub mod download;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureIndexResult {
    AlreadyPresent,
    Downloaded,
}

pub struct KicadSymbolsClient {
    conn: Connection,
}

impl KicadSymbolsClient {
    /// Get the default KiCad symbols database path (~/.pcb/kicad-symbols/symbols.db)
    pub fn default_db_path() -> Result<PathBuf> {
        let home = dirs::home_dir().context("Could not determine home directory")?;
        Ok(home.join(".pcb").join("kicad-symbols").join("symbols.db"))
    }

    /// Get the default KiCad symbols version sidecar path (~/.pcb/kicad-symbols/symbols.db.version)
    pub fn default_version_path() -> Result<PathBuf> {
        Ok(Self::default_db_path()?.with_extension("db.version"))
    }

    /// Returns true when the default KiCad symbols cache exists locally.
    pub fn is_cached() -> Result<bool> {
        Ok(Self::default_db_path()?.exists())
    }

    /// Returns the locally cached KiCad symbols version token, if present.
    pub fn local_version() -> Result<Option<String>> {
        let path = Self::default_db_path()?;
        Ok(download::load_local_version(&path))
    }

    /// Ensure the default KiCad symbols index exists locally.
    ///
    /// A prefetched metadata object can be provided to avoid a duplicate API request.
    pub fn ensure_cached(
        prefetched_metadata: Option<&download::KicadSymbolsIndexMetadata>,
    ) -> Result<EnsureIndexResult> {
        let path = Self::default_db_path()?;
        if path.exists() {
            return Ok(EnsureIndexResult::AlreadyPresent);
        }

        if let Some(metadata) = prefetched_metadata {
            let (progress_tx, progress_rx) = std::sync::mpsc::channel();
            let _ = progress_rx;
            download::download_kicad_symbols_index_with_progress(
                &path,
                &progress_tx,
                false,
                Some(metadata),
            )?;
        } else {
            download::download_kicad_symbols_index(&path)?;
        }

        Ok(EnsureIndexResult::Downloaded)
    }

    /// Refresh the default KiCad symbols index when the server-side version changes.
    pub fn refresh_if_stale() -> Result<download::RefreshResult> {
        let path = Self::default_db_path()?;
        download::refresh_kicad_symbols_index_if_stale(&path)
    }

    /// Open the KiCad symbols database from the default location.
    /// Downloads the index from the API server if not present locally.
    pub fn open() -> Result<Self> {
        let path = Self::default_db_path()?;
        Self::ensure_cached(None)?;
        Self::open_path(&path)
    }

    /// Open the KiCad symbols database from a specific path.
    pub fn open_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("KiCad symbols database not found at {}.", path.display());
        }

        // Register sqlite-vec extension BEFORE opening connection.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                unsafe extern "C" fn(
                    *mut rusqlite::ffi::sqlite3,
                    *mut *mut i8,
                    *const rusqlite::ffi::sqlite3_api_routines,
                ) -> i32,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }

        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .context("Failed to open KiCad symbols database")?;

        conn.execute_batch(
            "PRAGMA mmap_size = 268435456;  -- 256MB memory-mapped I/O
             PRAGMA cache_size = -65536;    -- 64MB page cache
             PRAGMA query_only = ON;",
        )
        .context("Failed to set read-only pragmas")?;

        Ok(Self { conn })
    }

    /// Get the total number of indexed symbols.
    pub fn count_symbols(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .map_err(Into::into)
    }
}
