use anyhow::{Context, Result};
use clap::Args;
use colored::Colorize;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write as IoWrite};
use std::path::{Path, PathBuf};

use crate::auth;

fn get_valid_token() -> Result<String> {
    let tokens =
        auth::load_tokens()?.context("Not authenticated. Run `pcb auth login` to authenticate.")?;

    // If token is expired, try to refresh it automatically
    if tokens.is_expired() {
        match auth::refresh_tokens() {
            Ok(new_tokens) => {
                println!("{}", "  Token refreshed".dimmed());
                return Ok(new_tokens.access_token);
            }
            Err(e) => {
                // Refresh failed - ask user to login again
                anyhow::bail!(
                    "Authentication token expired and refresh failed: {}\nRun `pcb auth login` to re-authenticate.",
                    e
                );
            }
        }
    }

    Ok(tokens.access_token)
}

fn get_api_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_API_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3001".to_string();
    #[cfg(not(debug_assertions))]
    return "https://api.diode.computer".to_string();
}

#[derive(Serialize)]
struct UploadUrlRequest {
    sha256: String,
    filename: String,
}

#[derive(Deserialize, Debug)]
struct UploadUrlResponse {
    #[serde(rename = "uploadUrl")]
    upload_url: Option<String>,
    #[serde(rename = "scanId")]
    scan_id: String,
}

#[derive(Serialize)]
struct ProcessRequest {
    #[serde(rename = "scanId")]
    scan_id: String,
    filename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ProcessResponse {
    #[serde(rename = "markdownUrl")]
    markdown_url: String,
    #[serde(rename = "imagesZipUrl")]
    images_zip_url: Option<String>,
    metadata: ProcessMetadata,
}

#[derive(Deserialize, Debug)]
struct ProcessMetadata {
    page_count: u32,
    image_count: u32,
    #[allow(dead_code)]
    timestamp: String,
    model: Option<String>,
    processing_time_ms: u32,
}

#[derive(Args, Debug)]
#[command(about = "Scan PDF datasheets with OCR")]
pub struct ScanArgs {
    /// PDF file to scan
    #[arg(value_name = "FILE")]
    file: PathBuf,

    /// Output directory (default: same directory as input file)
    #[arg(short, long, value_name = "DIR")]
    output: Option<PathBuf>,

    /// OCR model to use
    #[arg(short, long, value_name = "MODEL")]
    model: Option<String>,

    /// Download and extract images
    #[arg(long)]
    images: bool,
}

pub fn execute(args: ScanArgs) -> Result<()> {
    // Validate file exists
    if !args.file.exists() {
        anyhow::bail!("File not found: {}", args.file.display());
    }

    if args.file.extension().is_none_or(|ext| ext != "pdf") {
        anyhow::bail!("File must be a PDF: {}", args.file.display());
    }

    // Get output directory - default to same directory as input file
    let output_dir = args.output.unwrap_or_else(|| {
        args.file
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    });
    fs::create_dir_all(&output_dir).context("Failed to create output directory")?;

    // Get auth token
    let token = get_valid_token()?;

    // Get filename
    let filename = args
        .file
        .file_name()
        .context("Invalid filename")?
        .to_string_lossy()
        .to_string();

    println!("{} {}", "Scanning".dimmed(), filename.bold());

    // Calculate SHA256
    println!("  {} Calculating hash...", "→".dimmed());
    let sha256 = calculate_sha256(&args.file)?;

    // Create HTTP client with 3 minute timeout
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .context("Failed to create HTTP client")?;
    let base_url = get_api_base_url();

    // Request upload URL
    println!("  {} Requesting upload URL...", "→".dimmed());
    let upload_response = request_upload_url(&client, &token, &base_url, &sha256, &filename)?;

    // Upload PDF if needed
    if let Some(upload_url) = &upload_response.upload_url {
        println!("  {} Uploading PDF...", "→".dimmed());
        upload_pdf(&client, upload_url, &args.file)?;
    } else {
        println!("  {} PDF already exists, skipping upload", "✓".green());
    }

    // Request processing
    println!("  {} Processing PDF...", "→".dimmed());
    let process_response = request_process(
        &client,
        &token,
        &base_url,
        &upload_response.scan_id,
        &filename,
        args.model.as_deref(),
    )?;

    // Download markdown
    let md_filename = filename.replace(".pdf", ".md");
    let md_path = output_dir.join(&md_filename);
    println!("  {} Downloading markdown...", "→".dimmed());
    download_file(&client, &process_response.markdown_url, &md_path)?;

    // Download images if requested
    if args.images {
        if let Some(images_url) = &process_response.images_zip_url {
            println!("  {} Downloading images...", "→".dimmed());
            let images_zip_path = output_dir.join("images.zip");
            download_file(&client, images_url, &images_zip_path)?;

            // Extract images
            println!("  {} Extracting images...", "→".dimmed());
            let images_dir = output_dir.join("images");
            extract_zip(&images_zip_path, &images_dir)?;
            fs::remove_file(&images_zip_path)?;
        } else {
            println!("  {} No images found", "ℹ".blue());
        }
    }

    // Success message
    println!();
    println!("{}", "✓ Scan complete!".green().bold());
    println!("  Output: {}", md_path.display().to_string().cyan());
    println!(
        "  Pages: {} | Images: {} | Time: {:.1}s",
        process_response.metadata.page_count,
        process_response.metadata.image_count,
        process_response.metadata.processing_time_ms as f64 / 1000.0
    );
    if let Some(model) = &process_response.metadata.model {
        println!("  Model: {}", model.dimmed());
    }

    Ok(())
}

fn calculate_sha256(path: &Path) -> Result<String> {
    let mut file = File::open(path).context("Failed to open file")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = file.read(&mut buffer).context("Failed to read file")?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn request_upload_url(
    client: &Client,
    token: &str,
    base_url: &str,
    sha256: &str,
    filename: &str,
) -> Result<UploadUrlResponse> {
    let url = format!("{}/api/scan/upload-url", base_url);
    let body = UploadUrlRequest {
        sha256: sha256.to_string(),
        filename: filename.to_string(),
    };

    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .context("Failed to request upload URL")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().unwrap_or_default();
        anyhow::bail!("API error ({}): {}", status, error_text);
    }

    response
        .json::<UploadUrlResponse>()
        .context("Failed to parse upload URL response")
}

fn upload_pdf(client: &Client, upload_url: &str, file_path: &Path) -> Result<()> {
    let file_data = fs::read(file_path).context("Failed to read PDF file")?;

    let response = client
        .put(upload_url)
        .header("Content-Type", "application/pdf")
        .body(file_data)
        .send()
        .context("Failed to upload PDF")?;

    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!("Upload failed with status: {}", status);
    }

    Ok(())
}

fn request_process(
    client: &Client,
    token: &str,
    base_url: &str,
    scan_id: &str,
    filename: &str,
    model: Option<&str>,
) -> Result<ProcessResponse> {
    let url = format!("{}/api/scan/process", base_url);
    let body = ProcessRequest {
        scan_id: scan_id.to_string(),
        filename: filename.to_string(),
        model: model.map(|s| s.to_string()),
    };

    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .context("Failed to request processing")?;

    if !response.status().is_success() {
        let status = response.status();
        let error_text = response.text().unwrap_or_default();
        anyhow::bail!("API error ({}): {}", status, error_text);
    }

    response
        .json::<ProcessResponse>()
        .context("Failed to parse process response")
}

fn download_file(client: &Client, url: &str, path: &Path) -> Result<()> {
    let response = client.get(url).send().context("Failed to download file")?;

    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!("Download failed with status: {}", status);
    }

    let content = response.bytes().context("Failed to read response bytes")?;
    let mut file = File::create(path).context("Failed to create output file")?;
    file.write_all(&content)
        .context("Failed to write output file")?;

    Ok(())
}

fn extract_zip(zip_path: &Path, output_dir: &Path) -> Result<()> {
    let file = File::open(zip_path).context("Failed to open zip file")?;
    let mut archive = zip::ZipArchive::new(file).context("Failed to read zip archive")?;

    fs::create_dir_all(output_dir).context("Failed to create images directory")?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).context("Failed to read zip entry")?;
        let outpath = output_dir.join(file.name());

        if file.is_dir() {
            fs::create_dir_all(&outpath).context("Failed to create directory")?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent).context("Failed to create parent directory")?;
            }
            let mut outfile = File::create(&outpath).context("Failed to create file")?;
            std::io::copy(&mut file, &mut outfile).context("Failed to extract file")?;
        }
    }

    Ok(())
}
