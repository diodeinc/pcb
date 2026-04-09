use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use clap::{Args, ValueEnum};
use colored::Colorize;
use pcb_ui::Spinner;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use url::Url;

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
    pub images: bool,
}

pub fn scan_with_defaults(
    auth_token: &str,
    file: PathBuf,
    output: Option<PathBuf>,
    images: bool,
) -> Result<ScanResult> {
    let output_dir = output.unwrap_or(crate::datasheet::materialized_output_dir_for_pdf(&file)?);

    let options = ScanOptions {
        file,
        output_dir,
        images,
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

    let client = build_scan_client()?;

    let api_base_url = crate::get_api_base_url();
    let process_response = request_process(
        &client,
        auth_token,
        &api_base_url,
        Some(source_path),
        None,
        model.as_ref().map(|m| m.as_str()),
    )?;

    let result = materialize_scan_outputs(
        &client,
        &process_response,
        output_dir,
        filename,
        images,
        json,
    )?;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<String>,
    #[serde(rename = "sourceUrl")]
    #[serde(skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
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
    #[serde(rename = "sourcePdfUrl")]
    pub(crate) source_pdf_url: Option<String>,
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
    #[allow(dead_code)]
    ocr_cache_hit: Option<bool>,
}

fn with_spinner<F, R>(spinner: &Spinner, message: &str, f: F) -> Result<R>
where
    F: FnOnce() -> Result<R>,
{
    spinner.set_message(message.to_string());
    f()
}

pub(crate) fn build_scan_client() -> Result<Client> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(Into::into)
}

fn scan_result_from_process(
    output_path: PathBuf,
    process_response: &ProcessResponse,
) -> ScanResult {
    ScanResult {
        output_path,
        page_count: process_response.metadata.page_count,
        image_count: process_response.metadata.image_count,
        processing_time_ms: process_response.metadata.processing_time_ms,
        model: process_response.metadata.model.clone(),
    }
}

fn materialize_scan_outputs(
    client: &Client,
    process_response: &ProcessResponse,
    output_dir: &Path,
    filename: &str,
    images: bool,
    json: bool,
) -> Result<ScanResult> {
    let markdown_path = output_dir.join(filename.replace(".pdf", ".md"));
    let document_json_path = json.then(|| output_dir.join(filename.replace(".pdf", ".json")));
    let images_zip_path = images.then(|| output_dir.join("images.zip"));
    let images_dir = images.then(|| output_dir.join("images"));

    download_process_artifacts(
        client,
        process_response,
        &markdown_path,
        document_json_path.as_deref(),
        images_zip_path.as_deref(),
    )?;

    if let (Some(images_zip_path), Some(images_dir)) =
        (images_zip_path.as_deref(), images_dir.as_deref())
        && process_response.images_zip_url.is_some()
    {
        extract_images_archive(images_zip_path, images_dir)?;
    }

    Ok(scan_result_from_process(markdown_path, process_response))
}

pub fn scan_pdf(auth_token: &str, options: ScanOptions) -> Result<ScanResult> {
    validate_local_pdf_path(&options.file)?;

    fs::create_dir_all(&options.output_dir)?;

    let filename = options
        .file
        .file_name()
        .context("Invalid filename")?
        .to_string_lossy()
        .to_string();

    let spinner = Spinner::builder(format!("{}: Scanning", filename)).start();

    let sha256 = with_spinner(&spinner, "Calculating hash...", || {
        calculate_sha256(&options.file)
    })?;

    let client = build_scan_client()?;

    let process_response = with_spinner(&spinner, "Processing PDF...", || {
        process_local_pdf(&client, auth_token, &options.file, Some(&sha256), None)
    })?;

    let result = with_spinner(&spinner, "Materializing scan outputs...", || {
        materialize_scan_outputs(
            &client,
            &process_response,
            &options.output_dir,
            &filename,
            options.images,
            false,
        )
    })?;

    spinner.finish();

    Ok(result)
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

pub(crate) fn process_local_pdf(
    client: &Client,
    auth_token: &str,
    file_path: &Path,
    file_sha256: Option<&str>,
    model: Option<&str>,
) -> Result<ProcessResponse> {
    let api_base_url = crate::get_api_base_url();
    let filename = file_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Invalid filename")?;
    let sha256 = match file_sha256 {
        Some(sha256) => sha256.to_owned(),
        None => calculate_sha256(file_path)?,
    };

    let upload = request_upload_url(client, auth_token, &api_base_url, &sha256, filename)?;
    if let Some(upload_url) = upload.upload_url.as_deref() {
        upload_pdf(client, upload_url, file_path)?;
    }

    request_process(
        client,
        auth_token,
        &api_base_url,
        Some(&upload.source_path),
        None,
        model,
    )
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
    source_path: Option<&str>,
    source_url: Option<&str>,
    model: Option<&str>,
) -> Result<ProcessResponse> {
    if source_path.is_none() && source_url.is_none() {
        anyhow::bail!("Either source_path or source_url is required");
    }

    let url = format!("{}/api/scan/process", base_url);

    let response = client
        .post(&url)
        .bearer_auth(token)
        .json(&ProcessRequest {
            source_path: source_path.map(ToOwned::to_owned),
            source_url: source_url.map(ToOwned::to_owned),
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
    AtomicFile::new(path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(bytes.as_ref())?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Download write failed: {err}"))?;
    Ok(())
}

pub(crate) fn download_process_artifacts(
    client: &Client,
    process_response: &ProcessResponse,
    markdown_path: &Path,
    document_json_path: Option<&Path>,
    images_zip_path: Option<&Path>,
) -> Result<()> {
    std::thread::scope(|s| -> Result<()> {
        let md_handle =
            s.spawn(|| download_file(client, &process_response.markdown_url, markdown_path));

        let json_handle = process_response
            .document_json_url
            .as_ref()
            .zip(document_json_path)
            .map(|(url, path)| s.spawn(|| download_file(client, url, path)));

        let images_handle = process_response
            .images_zip_url
            .as_ref()
            .zip(images_zip_path)
            .map(|(url, path)| s.spawn(|| download_file(client, url, path)));

        md_handle.join().unwrap()?;
        if let Some(h) = json_handle {
            h.join().unwrap()?;
        }
        if let Some(h) = images_handle {
            h.join().unwrap()?;
        }
        Ok(())
    })
}

pub(crate) fn extract_images_archive(zip_path: &Path, output_dir: &Path) -> Result<()> {
    extract_zip(zip_path, output_dir)?;
    fs::remove_file(zip_path)?;
    Ok(())
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

enum ScanInput {
    LocalPdf(PathBuf),
    DatasheetUrl(String),
}

fn validate_local_pdf_path(path: &Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("File not found: {}", path.display());
    }

    if path.extension().is_none_or(|ext| ext != "pdf") {
        anyhow::bail!("File must be a PDF: {}", path.display());
    }

    Ok(())
}

fn parse_scan_input(input: &str) -> Result<ScanInput> {
    let lower = input.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let url = Url::parse(input).with_context(|| format!("Invalid URL input: {input}"))?;
        return Ok(ScanInput::DatasheetUrl(url.to_string()));
    }

    let path = PathBuf::from(input);
    validate_local_pdf_path(&path)?;

    Ok(ScanInput::LocalPdf(path))
}

#[derive(Args, Debug)]
#[command(about = "Scan datasheets from local PDFs or URLs")]
pub struct ScanArgs {
    #[arg(value_name = "INPUT")]
    pub input: String,

    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,
}

pub fn execute(args: ScanArgs) -> Result<()> {
    let input = parse_scan_input(&args.input)?;

    let token = crate::auth::get_valid_token()?;
    let (resolve_input, input_pdf_path) = match input {
        ScanInput::LocalPdf(file) => (
            crate::datasheet::ResolveDatasheetInput::PdfPath(file.clone()),
            Some(file),
        ),
        ScanInput::DatasheetUrl(url) => (
            crate::datasheet::ResolveDatasheetInput::DatasheetUrl(url),
            None,
        ),
    };
    let spinner = Spinner::builder("Resolving datasheet...").start();
    let response = crate::datasheet::resolve_datasheet(&token, &resolve_input)?;
    let pdf_path = input_pdf_path
        .unwrap_or_else(|| PathBuf::from(&response.pdf_path))
        .display()
        .to_string();
    let markdown_path = if let Some(output_dir) = args.output.as_deref() {
        crate::datasheet::copy_resolved_outputs(&response, output_dir, None, None)?
            .display()
            .to_string()
    } else {
        response.markdown_path
    };
    spinner.finish();

    println!("PDF: {pdf_path}");
    println!("Markdown: {markdown_path}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_scan_input_accepts_http_url() {
        let parsed = parse_scan_input("https://example.com/datasheet.pdf").unwrap();
        match parsed {
            ScanInput::DatasheetUrl(url) => {
                assert_eq!(url, "https://example.com/datasheet.pdf");
            }
            _ => panic!("expected URL input"),
        }
    }

    #[test]
    fn parse_scan_input_rejects_non_http_url() {
        assert!(parse_scan_input("ftp://example.com/datasheet.pdf").is_err());
    }

    #[test]
    fn parse_scan_input_windows_path_not_treated_as_url() {
        let err = match parse_scan_input(r"C:\__unlikely__\datasheet.pdf") {
            Ok(_) => panic!("expected local file validation error"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("File not found"));
        assert!(!err.contains("URL input must use http or https"));
    }

    #[test]
    fn parse_scan_input_windows_forward_slash_path_not_treated_as_url() {
        let err = match parse_scan_input("C:/__unlikely__/datasheet.pdf") {
            Ok(_) => panic!("expected local file validation error"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("File not found"));
        assert!(!err.contains("URL input must use http or https"));
    }

    #[test]
    fn parse_scan_input_accepts_local_pdf() {
        let file = std::env::temp_dir().join(format!("scan-local-{}.pdf", uuid::Uuid::new_v4()));
        fs::write(&file, b"%PDF-1.4\n").unwrap();

        let parsed = parse_scan_input(file.to_str().unwrap()).unwrap();
        match parsed {
            ScanInput::LocalPdf(path) => assert_eq!(path, file),
            _ => panic!("expected local PDF input"),
        }

        fs::remove_file(file).unwrap();
    }
}
