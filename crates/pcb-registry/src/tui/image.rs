//! Background image fetching worker with SQLite cache

use anyhow::{Context, Result};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

/// Request to fetch an image
#[derive(Debug, Clone)]
pub struct ImageRequest {
    pub url: String,
}

/// Response from image worker
pub enum ImageResponse {
    Loaded {
        url: String,
        protocol: StatefulProtocol,
    },
    Failed {
        url: String,
        error: String,
    },
}

/// State of an image in the loading lifecycle
pub enum ImageState {
    Loading { requested_at: std::time::Instant },
    Ready { protocol: StatefulProtocol },
    Failed { error: String },
}

/// Whether the terminal supports image display
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageProtocol {
    Kitty,
    None,
}

impl ImageProtocol {
    pub fn detect() -> Self {
        // Check for Ghostty or Kitty terminal
        if let Ok(term) = std::env::var("TERM") {
            if term.contains("kitty") || term.contains("ghostty") {
                return ImageProtocol::Kitty;
            }
        }
        // Ghostty also sets TERM_PROGRAM
        if let Ok(prog) = std::env::var("TERM_PROGRAM") {
            if prog.to_lowercase().contains("ghostty") {
                return ImageProtocol::Kitty;
            }
        }
        ImageProtocol::None
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self, ImageProtocol::None)
    }
}

/// Spawn the image worker thread
/// Takes a pre-created Picker from the main thread (to avoid blocking terminal queries in worker)
pub fn spawn_image_worker(
    request_rx: Receiver<ImageRequest>,
    response_tx: Sender<ImageResponse>,
    picker: Option<Picker>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let db_path = match default_image_db_path() {
            Ok(p) => p,
            Err(e) => {
                eprintln!("Image cache path error: {e}");
                // Continue without cache
                run_worker_loop(request_rx, response_tx, None, picker);
                return;
            }
        };

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let conn = match Connection::open(&db_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Failed to open image cache: {e}");
                run_worker_loop(request_rx, response_tx, None, picker);
                return;
            }
        };

        if let Err(e) = init_schema(&conn) {
            eprintln!("Failed to init image cache schema: {e}");
        }

        run_worker_loop(request_rx, response_tx, Some(conn), picker);
    })
}

/// Standard user agent for image fetches
const USER_AGENT: &str = "Mozilla/5.0 (compatible)";

/// Minimum interval between network requests (100ms = 10 req/sec)
const RATE_LIMIT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

fn run_worker_loop(
    request_rx: Receiver<ImageRequest>,
    response_tx: Sender<ImageResponse>,
    conn: Option<Connection>,
    picker: Option<Picker>,
) {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .user_agent(USER_AGENT)
        .build()
        .ok();

    let mut last_fetch_time: Option<std::time::Instant> = None;

    while let Ok(req) = request_rx.recv() {
        let url = req.url.clone();

        // Helper to process bytes into protocol
        let process_bytes = |bytes: &[u8], picker: &Picker| -> Result<StatefulProtocol> {
            let img = image::load_from_memory(bytes).context("Failed to decode image")?;
            Ok(picker.new_resize_protocol(img))
        };

        // 1. Try cache first
        if let Some(ref conn) = conn {
            if let Ok(Some(bytes)) = load_from_cache(conn, &url) {
                if let Some(ref p) = picker {
                    match process_bytes(&bytes, p) {
                        Ok(protocol) => {
                            let _ = response_tx.send(ImageResponse::Loaded { url, protocol });
                            continue;
                        }
                        Err(e) => {
                            let _ = response_tx.send(ImageResponse::Failed {
                                url,
                                error: e.to_string(),
                            });
                            continue;
                        }
                    }
                }
            }
        }

        // Rate limit network requests (not cache hits)
        if let Some(last) = last_fetch_time {
            let elapsed = last.elapsed();
            if elapsed < RATE_LIMIT_INTERVAL {
                std::thread::sleep(RATE_LIMIT_INTERVAL - elapsed);
            }
        }
        last_fetch_time = Some(std::time::Instant::now());

        // 2. Fetch from network
        match fetch_image_bytes(client.as_ref(), &url) {
            Ok(bytes) => {
                // 3. Store in cache (best-effort)
                if let Some(ref conn) = conn {
                    if let Err(e) = save_to_cache(conn, &url, &bytes) {
                        eprintln!("Failed to cache image {url}: {e}");
                    }
                }

                // 4. Decode and create protocol
                if let Some(ref p) = picker {
                    match process_bytes(&bytes, p) {
                        Ok(protocol) => {
                            let _ = response_tx.send(ImageResponse::Loaded { url, protocol });
                        }
                        Err(e) => {
                            let _ = response_tx.send(ImageResponse::Failed {
                                url,
                                error: e.to_string(),
                            });
                        }
                    }
                } else {
                    let _ = response_tx.send(ImageResponse::Failed {
                        url,
                        error: "No image protocol available".to_string(),
                    });
                }
            }
            Err(e) => {
                let _ = response_tx.send(ImageResponse::Failed {
                    url,
                    error: e.to_string(),
                });
            }
        }
    }
}

fn default_image_db_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("Could not determine home directory")?;
    Ok(home.join(".pcb").join("registry").join("image_cache.db"))
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS images (
            url TEXT PRIMARY KEY,
            content BLOB NOT NULL,
            content_type TEXT,
            fetched_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_images_fetched_at ON images(fetched_at);
        "#,
    )?;
    Ok(())
}

fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn load_from_cache(conn: &Connection, url: &str) -> Result<Option<Vec<u8>>> {
    let bytes: Option<Vec<u8>> = conn
        .query_row("SELECT content FROM images WHERE url = ?1", [url], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(bytes)
}

fn save_to_cache(conn: &Connection, url: &str, bytes: &[u8]) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO images (url, content, fetched_at) VALUES (?1, ?2, ?3)",
        params![url, bytes, now_unix_secs()],
    )?;
    Ok(())
}

fn fetch_image_bytes(client: Option<&reqwest::blocking::Client>, url: &str) -> Result<Vec<u8>> {
    let client = client.context("HTTP client not available")?;
    let resp = client.get(url).send().context("Failed to fetch image")?;

    if !resp.status().is_success() {
        anyhow::bail!("HTTP {}", resp.status());
    }

    let bytes = resp.bytes().context("Failed to read image bytes")?;
    Ok(bytes.to_vec())
}
