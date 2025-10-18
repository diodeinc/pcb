use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
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

pub fn scan_with_defaults(
    auth_token: &str,
    file: PathBuf,
    output: Option<PathBuf>,
    model: Option<ScanModel>,
    images: bool,
) -> Result<ScanResult> {
    let output_dir = output.unwrap_or_else(|| {
        file.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    });

    let options = ScanOptions {
        file,
        output_dir,
        model,
        images,
    };

    scan_pdf(auth_token, options)
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

#[derive(Deserialize)]
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

#[derive(Deserialize)]
struct ProcessResponse {
    #[serde(rename = "markdownUrl")]
    markdown_url: String,
    #[serde(rename = "imagesZipUrl")]
    images_zip_url: Option<String>,
    metadata: ProcessMetadata,
}

#[derive(Deserialize)]
struct ProcessMetadata {
    page_count: u32,
    image_count: u32,
    #[allow(dead_code)]
    timestamp: String,
    model: Option<String>,
    processing_time_ms: u32,
}

fn with_spinner<F, R>(message: &str, completion: &str, f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    use indicatif::ProgressBar;
    let spinner = ProgressBar::new_spinner();
    spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    spinner.set_message(format!(" {}", message));
    let result = f()?;
    spinner.finish_and_clear();
    println!("  {} {}", "✓".green(), completion);
    Ok(result)
}

pub fn scan_pdf(auth_token: &str, options: ScanOptions) -> Result<ScanResult> {
    if !options.file.exists() {
        anyhow::bail!("File not found: {}", options.file.display());
    }

    if options.file.extension().is_none_or(|ext| ext != "pdf") {
        anyhow::bail!("File must be a PDF: {}", options.file.display());
    }

    fs::create_dir_all(&options.output_dir)?;

    let filename = options
        .file
        .file_name()
        .context("Invalid filename")?
        .to_string_lossy()
        .to_string();

    println!("\n{} {}", "Scanning".green().bold(), filename.bold());

    let sha256 = with_spinner("Calculating hash...", "Hash calculated", || {
        calculate_sha256(&options.file)
    })?;

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;

    let api_base_url = crate::get_api_base_url();

    let upload_response = with_spinner("Requesting upload URL...", "Upload URL received", || {
        request_upload_url(&client, auth_token, &api_base_url, &sha256, &filename)
    })?;

    if let Some(upload_url) = &upload_response.upload_url {
        with_spinner("Uploading PDF...", "PDF uploaded", || {
            upload_pdf(&client, upload_url, &options.file)
        })?;
    } else {
        println!("  {} PDF already exists, skipping upload", "✓".green());
    }

    let process_response = with_spinner("Processing PDF...", "PDF processed", || {
        request_process(
            &client,
            auth_token,
            &api_base_url,
            &upload_response.scan_id,
            &filename,
            options.model.as_ref().map(|m| m.as_str()),
        )
    })?;

    let md_filename = filename.replace(".pdf", ".md");
    let md_path = options.output_dir.join(&md_filename);

    with_spinner("Downloading markdown...", "Markdown downloaded", || {
        download_file(&client, &process_response.markdown_url, &md_path)
    })?;

    if options.images {
        if let Some(images_url) = &process_response.images_zip_url {
            let images_zip_path = options.output_dir.join("images.zip");
            with_spinner("Downloading images...", "Images downloaded", || {
                download_file(&client, images_url, &images_zip_path)
            })?;

            let images_dir = options.output_dir.join("images");
            with_spinner("Extracting images...", "Images extracted", || {
                extract_zip(&images_zip_path, &images_dir)?;
                fs::remove_file(&images_zip_path)?;
                Ok(())
            })?;
        } else {
            println!("  {} No images found", "ℹ".blue());
        }
    }

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
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = file.read(&mut buffer)?;
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

    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&UploadUrlRequest {
            sha256: sha256.to_string(),
            filename: filename.to_string(),
        })
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("API error: {}", response.status());
    }

    Ok(response.json()?)
}

fn upload_pdf(client: &Client, upload_url: &str, file_path: &Path) -> Result<()> {
    let file_data = fs::read(file_path)?;

    let response = client
        .put(upload_url)
        .header("Content-Type", "application/pdf")
        .body(file_data)
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("Upload failed: {}", response.status());
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

    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&ProcessRequest {
            scan_id: scan_id.to_string(),
            filename: filename.to_string(),
            model: model.map(|s| s.to_string()),
        })
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("API error: {}", response.status());
    }

    Ok(response.json()?)
}

fn download_file(client: &Client, url: &str, path: &Path) -> Result<()> {
    let response = client.get(url).send()?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed: {}", response.status());
    }

    let mut file = File::create(path)?;
    file.write_all(&response.bytes()?)?;
    Ok(())
}

fn extract_zip(zip_path: &Path, output_dir: &Path) -> Result<()> {
    let file = File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    fs::create_dir_all(output_dir)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let outpath = output_dir.join(file.name());

        if file.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(parent) = outpath.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut outfile = File::create(&outpath)?;
            std::io::copy(&mut file, &mut outfile)?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScanModelArg {
    #[value(name = "mistral-ocr-latest")]
    MistralOcrLatest,
    #[value(name = "gpt-4o")]
    Gpt4o,
    #[value(name = "gpt-4o-mini")]
    Gpt4oMini,
}

impl From<ScanModelArg> for ScanModel {
    fn from(arg: ScanModelArg) -> Self {
        match arg {
            ScanModelArg::MistralOcrLatest => ScanModel::MistralOcrLatest,
            ScanModelArg::Gpt4o => ScanModel::Gpt4o,
            ScanModelArg::Gpt4oMini => ScanModel::Gpt4oMini,
        }
    }
}

#[derive(Args, Debug)]
#[command(about = "Scan PDF datasheets with OCR")]
pub struct ScanArgs {
    #[arg(value_name = "FILE")]
    pub file: PathBuf,

    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,

    #[arg(short, long, value_enum)]
    pub model: Option<ScanModelArg>,

    #[arg(long)]
    pub images: bool,
}

pub fn execute(args: ScanArgs) -> Result<()> {
    if args.images && !matches!(args.model, None | Some(ScanModelArg::MistralOcrLatest)) {
        anyhow::bail!("The --images flag is only supported with the mistral-ocr-latest model");
    }

    let token = crate::auth::get_valid_token()?;
    scan_with_defaults(
        &token,
        args.file,
        args.output,
        args.model.map(Into::into),
        args.images,
    )?;
    Ok(())
}
