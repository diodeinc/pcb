use anyhow::{Context, Result};
use clap::{Args, ValueEnum};
use colored::Colorize;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanModel {
    MistralOcr2512,
    DatalabFast,
    DatalabBalanced,
    DatalabAccurate,
}

impl ScanModel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MistralOcr2512 => "mistral-ocr-2512",
            Self::DatalabFast => "datalab-fast",
            Self::DatalabBalanced => "datalab-balanced",
            Self::DatalabAccurate => "datalab-accurate",
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
            "mistral-ocr-2512" => Ok(Self::MistralOcr2512),
            "datalab-fast" => Ok(Self::DatalabFast),
            "datalab-balanced" => Ok(Self::DatalabBalanced),
            "datalab-accurate" => Ok(Self::DatalabAccurate),
            _ => anyhow::bail!(
                "Invalid model: {}. Valid options: mistral-ocr-2512, datalab-fast, datalab-balanced, datalab-accurate",
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
    pub json: bool,
}

pub fn scan_with_defaults(
    auth_token: &str,
    file: PathBuf,
    output: Option<PathBuf>,
    model: Option<ScanModel>,
    images: bool,
    json: bool,
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
        json,
    };

    scan_pdf(auth_token, options)
}

/// Scan a PDF that already exists in Supabase storage (no upload needed)
///
/// # Arguments
/// * `auth_token` - Authentication token
/// * `source_path` - Path in Supabase storage, e.g. "components/cse/Bosch/BMI323/datasheet.pdf"
/// * `output_dir` - Directory to save markdown and images
/// * `model` - Optional model to use for OCR
/// * `images` - Whether to download and extract images
/// * `json` - Whether to download the document JSON
/// * `show_output` - Whether to show progress and completion output
pub fn scan_from_source_path(
    auth_token: &str,
    source_path: &str,
    output_dir: impl AsRef<Path>,
    model: Option<ScanModel>,
    images: bool,
    json: bool,
    show_output: bool,
) -> Result<ScanResult> {
    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)?;

    let filename = source_path
        .split('/')
        .next_back()
        .context("Invalid source_path")?;

    if show_output {
        println!(
            "\n{} {} (from {})",
            "Scanning".green().bold(),
            filename.bold(),
            source_path.dimmed()
        );
    }

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;

    let api_base_url = crate::get_api_base_url();
    let process_response = request_process(
        &client,
        auth_token,
        &api_base_url,
        source_path,
        model.as_ref().map(|m| m.as_str()),
    )?;

    let md_path = output_dir.join(filename.replace(".pdf", ".md"));
    let json_path = output_dir.join(filename.replace(".pdf", ".json"));
    let images_zip_path = output_dir.join("images.zip");

    // Download markdown, JSON, and images in parallel
    std::thread::scope(|s| -> Result<()> {
        let md_handle =
            s.spawn(|| download_file(&client, &process_response.markdown_url, &md_path));

        let json_handle = process_response
            .document_json_url
            .as_ref()
            .filter(|_| json)
            .map(|url| s.spawn(|| download_file(&client, url, &json_path)));

        let images_handle = process_response
            .images_zip_url
            .as_ref()
            .filter(|_| images)
            .map(|url| s.spawn(|| download_file(&client, url, &images_zip_path)));

        md_handle.join().unwrap()?;
        if let Some(h) = json_handle {
            h.join().unwrap()?;
        }
        if let Some(h) = images_handle {
            h.join().unwrap()?;
        }
        Ok(())
    })?;

    // Extract images
    if images && process_response.images_zip_url.is_some() {
        extract_zip(&images_zip_path, &output_dir.join("images"))?;
        fs::remove_file(&images_zip_path)?;
    }

    let result = ScanResult {
        output_path: md_path,
        page_count: process_response.metadata.page_count,
        image_count: process_response.metadata.image_count,
        processing_time_ms: process_response.metadata.processing_time_ms,
        model: process_response.metadata.model,
    };

    if show_output {
        println!();
        println!("{}", "✓ Scan complete!".green().bold());
        println!(
            "  Output: {}",
            result.output_path.display().to_string().cyan()
        );
        println!(
            "  Pages: {} | Images: {} | Time: {:.1}s",
            result.page_count,
            result.image_count,
            result.processing_time_ms as f64 / 1000.0
        );
        if let Some(model) = &result.model {
            println!("  Model: {}", model.dimmed());
        }
    }

    Ok(result)
}

#[derive(Debug)]
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
pub(crate) struct UploadUrlResponse {
    #[serde(rename = "uploadUrl")]
    pub(crate) upload_url: Option<String>,
    #[serde(rename = "sourcePath")]
    pub(crate) source_path: String,
}

#[derive(Serialize)]
struct ProcessRequest {
    #[serde(rename = "sourcePath")]
    source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    concurrency: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct ProcessResponse {
    #[serde(rename = "markdownUrl")]
    pub(crate) markdown_url: String,
    #[serde(rename = "documentJsonUrl")]
    pub(crate) document_json_url: Option<String>,
    #[serde(rename = "imagesZipUrl")]
    pub(crate) images_zip_url: Option<String>,
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
            &upload_response.source_path,
            options.model.as_ref().map(|m| m.as_str()),
        )
    })?;

    let md_filename = filename.replace(".pdf", ".md");
    let md_path = options.output_dir.join(&md_filename);

    with_spinner("Downloading markdown...", "Markdown downloaded", || {
        download_file(&client, &process_response.markdown_url, &md_path)
    })?;

    if options.json
        && let Some(json_url) = &process_response.document_json_url
    {
        let json_filename = filename.replace(".pdf", ".json");
        let json_path = options.output_dir.join(&json_filename);
        with_spinner("Downloading JSON...", "JSON downloaded", || {
            download_file(&client, json_url, &json_path)
        })?;
    }

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

pub(crate) fn calculate_sha256(path: &Path) -> Result<String> {
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

pub(crate) fn request_upload_url(
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

pub(crate) fn upload_pdf(client: &Client, upload_url: &str, file_path: &Path) -> Result<()> {
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

pub(crate) fn request_process(
    client: &Client,
    token: &str,
    base_url: &str,
    source_path: &str,
    model: Option<&str>,
) -> Result<ProcessResponse> {
    let url = format!("{}/api/scan/process", base_url);

    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&ProcessRequest {
            source_path: source_path.to_string(),
            model: model.map(|s| s.to_string()),
            concurrency: None,
        })
        .send()?;

    if !response.status().is_success() {
        anyhow::bail!("API error: {}", response.status());
    }

    Ok(response.json()?)
}

pub(crate) fn download_file(client: &Client, url: &str, path: &Path) -> Result<()> {
    let response = client.get(url).send()?;

    if !response.status().is_success() {
        anyhow::bail!("Download failed: {}", response.status());
    }

    let bytes = response.bytes()?;
    write_bytes_atomically(path, bytes.as_ref())
}

pub(crate) fn extract_zip(zip_path: &Path, output_dir: &Path) -> Result<()> {
    let file = File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(file)?;

    fs::create_dir_all(output_dir)?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let Some(enclosed_name) = file.enclosed_name().map(|p| p.to_owned()) else {
            continue;
        };
        let outpath = output_dir.join(enclosed_name);

        if file.is_dir() {
            fs::create_dir_all(&outpath)?;
            continue;
        }

        if let Some(parent) = outpath.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut outfile = File::create(&outpath)?;
        std::io::copy(&mut file, &mut outfile)?;
    }

    Ok(())
}

pub(crate) fn write_bytes_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .context(format!("No parent directory for {}", path.display()))?;
    fs::create_dir_all(parent)?;

    let file_stem = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp_path = parent.join(format!(".{file_stem}.tmp-{}-{nonce}", std::process::id()));

    fs::write(&temp_path, bytes)?;
    fs::rename(&temp_path, path).inspect_err(|_err| {
        let _ = fs::remove_file(&temp_path);
    })?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScanModelArg {
    #[value(name = "mistral-ocr-2512")]
    MistralOcr2512,
    #[value(name = "datalab-fast")]
    DatalabFast,
    #[value(name = "datalab-balanced")]
    DatalabBalanced,
    #[value(name = "datalab-accurate")]
    DatalabAccurate,
}

impl From<ScanModelArg> for ScanModel {
    fn from(arg: ScanModelArg) -> Self {
        match arg {
            ScanModelArg::MistralOcr2512 => ScanModel::MistralOcr2512,
            ScanModelArg::DatalabFast => ScanModel::DatalabFast,
            ScanModelArg::DatalabBalanced => ScanModel::DatalabBalanced,
            ScanModelArg::DatalabAccurate => ScanModel::DatalabAccurate,
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

    /// Skip downloading images from the scanned PDF
    #[arg(long)]
    pub no_images: bool,

    /// Download the document JSON in addition to markdown
    #[arg(long)]
    pub json: bool,
}

pub fn execute(args: ScanArgs) -> Result<()> {
    let token = crate::auth::get_valid_token()?;
    scan_with_defaults(
        &token,
        args.file,
        args.output,
        args.model.map(Into::into),
        !args.no_images,
        args.json,
    )?;
    Ok(())
}
