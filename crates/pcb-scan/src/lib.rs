use anyhow::{Context, Result};
use colored::Colorize;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write as IoWrite};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanModel {
    MistralOcrLatest,
    Gpt4o,
    Gpt4oMini,
}

impl ScanModel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MistralOcrLatest => "mistral-ocr-latest",
            Self::Gpt4o => "gpt-4o",
            Self::Gpt4oMini => "gpt-4o-mini",
        }
    }
}

impl std::fmt::Display for ScanModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ScanModel {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "mistral-ocr-latest" => Ok(Self::MistralOcrLatest),
            "gpt-4o" => Ok(Self::Gpt4o),
            "gpt-4o-mini" => Ok(Self::Gpt4oMini),
            _ => anyhow::bail!(
                "Invalid model: {}. Valid options: mistral-ocr-latest, gpt-4o, gpt-4o-mini",
                s
            ),
        }
    }
}

pub struct ScanOptions {
    pub file: PathBuf,
    pub output_dir: PathBuf,
    pub model: Option<ScanModel>,
    pub images: bool,
}

pub struct ScanResult {
    pub output_path: PathBuf,
    pub page_count: u32,
    pub image_count: u32,
    pub processing_time_ms: u32,
    pub model: Option<String>,
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

pub fn scan_pdf(api_base_url: &str, auth_token: &str, options: ScanOptions) -> Result<ScanResult> {
    // Validate file exists
    if !options.file.exists() {
        anyhow::bail!("File not found: {}", options.file.display());
    }

    if options.file.extension().is_none_or(|ext| ext != "pdf") {
        anyhow::bail!("File must be a PDF: {}", options.file.display());
    }

    // Create output directory
    fs::create_dir_all(&options.output_dir).context("Failed to create output directory")?;

    // Get filename
    let filename = options
        .file
        .file_name()
        .context("Invalid filename")?
        .to_string_lossy()
        .to_string();

    println!("{} {}", "Scanning".dimmed(), filename.bold());

    // Calculate SHA256
    println!("  {} Calculating hash...", "→".dimmed());
    let sha256 = calculate_sha256(&options.file)?;

    // Create HTTP client with 3 minute timeout
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .context("Failed to create HTTP client")?;

    // Request upload URL
    println!("  {} Requesting upload URL...", "→".dimmed());
    let upload_response =
        request_upload_url(&client, auth_token, api_base_url, &sha256, &filename)?;

    // Upload PDF if needed
    if let Some(upload_url) = &upload_response.upload_url {
        println!("  {} Uploading PDF...", "→".dimmed());
        upload_pdf(&client, upload_url, &options.file)?;
    } else {
        println!("  {} PDF already exists, skipping upload", "✓".green());
    }

    // Request processing
    println!("  {} Processing PDF...", "→".dimmed());
    let process_response = request_process(
        &client,
        auth_token,
        api_base_url,
        &upload_response.scan_id,
        &filename,
        options.model.as_ref().map(|m| m.as_str()),
    )?;

    // Download markdown
    let md_filename = filename.replace(".pdf", ".md");
    let md_path = options.output_dir.join(&md_filename);
    println!("  {} Downloading markdown...", "→".dimmed());
    download_file(&client, &process_response.markdown_url, &md_path)?;

    // Download images if requested
    if options.images {
        if let Some(images_url) = &process_response.images_zip_url {
            println!("  {} Downloading images...", "→".dimmed());
            let images_zip_path = options.output_dir.join("images.zip");
            download_file(&client, images_url, &images_zip_path)?;

            // Extract images
            println!("  {} Extracting images...", "→".dimmed());
            let images_dir = options.output_dir.join("images");
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

    Ok(ScanResult {
        output_path: md_path,
        page_count: process_response.metadata.page_count,
        image_count: process_response.metadata.image_count,
        processing_time_ms: process_response.metadata.processing_time_ms,
        model: process_response.metadata.model,
    })
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
