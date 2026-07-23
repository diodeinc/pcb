use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use clap::Args;
use pcb_ui::Spinner;
use reqwest::blocking::{Client, multipart};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use url::Url;

const MAX_SCAN_PAGES: u32 = 1_000;

#[derive(Serialize)]
struct CreateDatasheetRequest {
    url: String,
}

#[derive(Deserialize)]
pub(crate) struct DatasheetResponse {
    pub(crate) id: String,
    #[serde(rename = "fileUrl")]
    pub(crate) file_url: String,
    pub(crate) sha256: String,
    pub(crate) filename: String,
}

#[derive(Deserialize)]
pub(crate) struct DatasheetScanResponse {
    #[serde(rename = "markdownUrl")]
    pub(crate) markdown_url: String,
    #[serde(rename = "imagesZipUrl")]
    pub(crate) images_zip_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct PageRange {
    start: u32,
    end: u32,
}

impl PageRange {
    pub(crate) fn start(self) -> u32 {
        self.start
    }

    pub(crate) fn end(self) -> u32 {
        self.end
    }

    fn page_count(self) -> u32 {
        self.end - self.start + 1
    }
}

impl std::fmt::Display for PageRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.start, self.end)
    }
}

impl FromStr for PageRange {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        let (start, end) = value.split_once('-').ok_or_else(|| {
            "page range must use START-END with one-based inclusive page numbers".to_string()
        })?;
        let start = start.parse::<u32>().map_err(|_| {
            "page range must use START-END with one-based inclusive page numbers".to_string()
        })?;
        let end = end.parse::<u32>().map_err(|_| {
            "page range must use START-END with one-based inclusive page numbers".to_string()
        })?;

        if start == 0 || end == 0 {
            return Err(
                "page range uses one-based page numbers; START and END must be at least 1"
                    .to_string(),
            );
        }
        if end < start {
            return Err("page range END must be greater than or equal to START".to_string());
        }

        let range = Self { start, end };
        if range.page_count() > MAX_SCAN_PAGES {
            return Err(format!(
                "page range selects {} pages; at most {MAX_SCAN_PAGES} pages can be scanned",
                range.page_count()
            ));
        }

        Ok(range)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScanDatasheetRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    page_range: Option<PageRange>,
}

pub(crate) enum DatasheetScanOutcome {
    Scanned(DatasheetScanResponse),
    NotCached,
}

pub(crate) fn datasheet_id_for_sha256(sha256: &str) -> String {
    format!("sha256:{sha256}")
}

fn api_error(context: &str, response: reqwest::blocking::Response) -> anyhow::Error {
    #[derive(Deserialize)]
    struct ApiError {
        error: String,
    }

    let status = response.status();
    match response.json::<ApiError>() {
        Ok(body) if !body.error.is_empty() => {
            anyhow::anyhow!("{context} failed ({status}): {}", body.error)
        }
        _ => anyhow::anyhow!("{context} failed: {status}"),
    }
}

pub(crate) fn build_scan_client() -> Result<Client> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(Into::into)
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

    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn create_datasheet_from_url(
    client: &Client,
    token: Option<&str>,
    url: &str,
) -> Result<DatasheetResponse> {
    let endpoint = format!("{}/api/datasheets", crate::get_api_base_url());

    let response = crate::auth::apply_bearer_auth(client.post(&endpoint), token)
        .json(&CreateDatasheetRequest {
            url: url.to_string(),
        })
        .send()?;

    if !response.status().is_success() {
        return Err(api_error("Datasheet download", response));
    }

    Ok(response.json()?)
}

pub(crate) fn create_datasheet_from_pdf(
    client: &Client,
    token: Option<&str>,
    pdf_path: &Path,
) -> Result<DatasheetResponse> {
    let endpoint = format!("{}/api/datasheets", crate::get_api_base_url());
    let form = multipart::Form::new()
        .file("file", pdf_path)
        .with_context(|| {
            format!(
                "Failed to read datasheet PDF for upload: {}",
                pdf_path.display()
            )
        })?;

    let response = crate::auth::apply_bearer_auth(client.post(&endpoint), token)
        .multipart(form)
        .send()?;

    if !response.status().is_success() {
        return Err(api_error("Datasheet upload", response));
    }

    Ok(response.json()?)
}

pub(crate) fn scan_datasheet(
    client: &Client,
    token: Option<&str>,
    datasheet_id: &str,
    page_range: Option<PageRange>,
) -> Result<DatasheetScanOutcome> {
    let endpoint = format!(
        "{}/api/datasheets/{}/scan",
        crate::get_api_base_url(),
        datasheet_id
    );

    let response = crate::auth::apply_bearer_auth(client.post(&endpoint), token)
        .json(&ScanDatasheetRequest { page_range })
        .send()?;

    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(DatasheetScanOutcome::NotCached);
    }
    if !response.status().is_success() {
        return Err(api_error("Datasheet scan", response));
    }

    Ok(DatasheetScanOutcome::Scanned(response.json()?))
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

pub(crate) fn download_scan_artifacts(
    client: &Client,
    scan_response: &DatasheetScanResponse,
    markdown_path: &Path,
    images_zip_path: Option<&Path>,
) -> Result<()> {
    std::thread::scope(|s| -> Result<()> {
        let md_handle =
            s.spawn(|| download_file(client, &scan_response.markdown_url, markdown_path));

        let images_handle = scan_response
            .images_zip_url
            .as_ref()
            .zip(images_zip_path)
            .map(|(url, path)| s.spawn(|| download_file(client, url, path)));

        md_handle.join().unwrap()?;
        if let Some(h) = images_handle {
            h.join().unwrap()?;
        }
        Ok(())
    })
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

    /// Scan a one-based, inclusive page range (for example, 1-1000)
    #[arg(long, value_name = "START-END")]
    pages: Option<PageRange>,
}

pub fn execute(args: ScanArgs) -> Result<()> {
    let input = parse_scan_input(&args.input)?;

    let token = crate::auth::get_api_token()?;
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
    let response =
        crate::datasheet::resolve_datasheet(token.as_deref(), &resolve_input, args.pages)?;
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
    if let Some(pages) = args.pages {
        println!("Pages: {pages}");
    }

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

    #[test]
    fn page_range_is_one_based_inclusive() {
        let range = "1-1000".parse::<PageRange>().unwrap();
        assert_eq!(range.start(), 1);
        assert_eq!(range.end(), 1000);
        assert_eq!(range.page_count(), 1000);
    }

    #[test]
    fn page_range_rejects_invalid_ranges() {
        for value in ["1", "a-b", "0-1", "2-1", "1-1001"] {
            assert!(
                value.parse::<PageRange>().is_err(),
                "expected {value:?} to be rejected"
            );
        }
    }

    #[test]
    fn scan_request_omits_page_range_for_full_scan() {
        let request = ScanDatasheetRequest { page_range: None };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn scan_request_serializes_page_range() {
        let request = ScanDatasheetRequest {
            page_range: Some("7-12".parse().unwrap()),
        };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "pageRange": {
                    "start": 7,
                    "end": 12
                }
            })
        );
    }
}
