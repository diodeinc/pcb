use anyhow::{Context, Result};
use chrono::Utc;
use pcb_zen::cache_index::cache_base;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;
use uuid::Uuid;

use crate::scan::{
    calculate_sha256, download_file, extract_zip, request_process, request_upload_url, upload_pdf,
};

const DATASHEET_NAMESPACE_UUID: &str = "fe255507-b3f4-4ec0-98cb-9e3f90cfd8eb";
const DATASHEET_DOWNLOAD_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone)]
pub enum ResolveDatasheetInput {
    DatasheetUrl(String),
    PdfPath(PathBuf),
    KicadSymPath {
        path: PathBuf,
        symbol_name: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolveDatasheetResponse {
    pub markdown_path: String,
    pub images_dir: String,
    pub pdf_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub datasheet_url: Option<String>,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
struct UrlPdfMetadata {
    original_url: String,
    canonical_url: String,
    downloaded_at: String,
    content_sha256: String,
}

pub fn parse_resolve_request(args: Option<&Value>) -> Result<ResolveDatasheetInput> {
    let args = args.ok_or_else(|| anyhow::anyhow!("arguments required"))?;

    let datasheet_url = optional_trimmed_string(args, "datasheet_url");
    let pdf_path = optional_path(args, "pdf_path");
    let kicad_sym_path = optional_path(args, "kicad_sym_path");
    let symbol_name = optional_trimmed_string(args, "symbol_name");

    if symbol_name.is_some() && kicad_sym_path.is_none() {
        anyhow::bail!("symbol_name requires kicad_sym_path");
    }

    match (datasheet_url, pdf_path, kicad_sym_path) {
        (Some(url), None, None) => Ok(ResolveDatasheetInput::DatasheetUrl(url)),
        (None, Some(path), None) => {
            validate_local_pdf(&path)?;
            Ok(ResolveDatasheetInput::PdfPath(path))
        }
        (None, None, Some(path)) => {
            validate_local_kicad_sym(&path)?;
            Ok(ResolveDatasheetInput::KicadSymPath { path, symbol_name })
        }
        _ => {
            anyhow::bail!("Exactly one of datasheet_url, pdf_path, kicad_sym_path must be provided")
        }
    }
}

pub fn resolve_datasheet(
    auth_token: &str,
    input: &ResolveDatasheetInput,
) -> Result<ResolveDatasheetResponse> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(180))
        .build()?;

    let (pdf_path, datasheet_url) = match input {
        ResolveDatasheetInput::DatasheetUrl(url) => {
            let (pdf_path, canonical_url) = resolve_pdf_from_url(&client, url)?;
            (pdf_path, Some(canonical_url))
        }
        ResolveDatasheetInput::PdfPath(path) => (path.clone(), None),
        ResolveDatasheetInput::KicadSymPath { path, symbol_name } => {
            let url = extract_datasheet_url_from_kicad_sym(path, symbol_name.as_deref())?;
            let (pdf_path, canonical_url) = resolve_pdf_from_url(&client, &url)?;
            (pdf_path, Some(canonical_url))
        }
    };

    let pdf_sha256 = calculate_sha256(&pdf_path)?;
    let materialization_id = materialization_id_for_sha(&pdf_sha256)?;
    let materialized_dir = materialized_dir(&materialization_id);
    let markdown_path = materialized_dir.join("datasheet.md");
    let images_dir = materialized_dir.join("images");
    let api_base_url = crate::get_api_base_url();

    if markdown_path.exists() {
        fs::create_dir_all(&images_dir)?;
        return Ok(build_resolve_response(
            &markdown_path,
            &images_dir,
            &pdf_path,
            datasheet_url,
            pdf_sha256,
        ));
    }

    fs::create_dir_all(&materialized_dir)?;

    let filename = inferred_pdf_filename(&pdf_path);
    let upload = request_upload_url(&client, auth_token, &api_base_url, &pdf_sha256, &filename)?;

    if let Some(upload_url) = upload.upload_url.as_deref() {
        upload_pdf(&client, upload_url, &pdf_path)?;
    }

    let process = request_process(
        &client,
        auth_token,
        &api_base_url,
        &upload.source_path,
        None,
    )?;

    download_file(&client, &process.markdown_url, &markdown_path)
        .context("Failed to download markdown output")?;

    if let Some(images_zip_url) = process.images_zip_url.as_deref() {
        let zip_path = materialized_dir.join("images.zip");
        download_file(&client, images_zip_url, &zip_path)
            .context("Failed to download image archive")?;
        extract_zip(&zip_path, &images_dir)?;
        fs::remove_file(zip_path)?;
    } else {
        fs::create_dir_all(&images_dir)?;
    }

    Ok(build_resolve_response(
        &markdown_path,
        &images_dir,
        &pdf_path,
        datasheet_url,
        pdf_sha256,
    ))
}

fn resolve_pdf_from_url(client: &Client, url: &str) -> Result<(PathBuf, String)> {
    let canonical_url = canonicalize_url(url)?;
    let key = sha256_hex(canonical_url.as_bytes());
    let cache_dir = url_pdf_cache_dir();
    fs::create_dir_all(&cache_dir)?;

    let pdf_path = cache_dir.join(format!("{key}.pdf"));
    let metadata_path = cache_dir.join(format!("{key}.json"));
    if pdf_path.exists() {
        return Ok((pdf_path, canonical_url));
    }

    let parsed_url = Url::parse(&canonical_url)
        .with_context(|| format!("Failed to parse canonical URL {canonical_url}"))?;
    let headers = datasheet_download_headers(&parsed_url)?;

    // First attempt with the default client; fallback to HTTP/1.1 for flaky servers.
    let (bytes, content_type) = match fetch_pdf_with_headers(
        client,
        &canonical_url,
        headers.clone(),
    ) {
        Ok(result) => result,
        Err(first_err) => {
            let http1_client = Client::builder()
                .timeout(std::time::Duration::from_secs(
                    DATASHEET_DOWNLOAD_TIMEOUT_SECS,
                ))
                .http1_only()
                .build()
                .context("Failed to build HTTP/1.1 fallback client")?;
            fetch_pdf_with_headers(&http1_client, &canonical_url, headers).with_context(|| {
                format!(
                    "Failed to download datasheet from {canonical_url} (default attempt error: {first_err})"
                )
            })?
        }
    };

    if !looks_like_pdf(&bytes, content_type.as_deref()) {
        anyhow::bail!("Downloaded datasheet is not a PDF");
    }

    fs::write(&pdf_path, &bytes)?;
    let metadata = UrlPdfMetadata {
        original_url: url.to_string(),
        canonical_url: canonical_url.clone(),
        downloaded_at: Utc::now().to_rfc3339(),
        content_sha256: sha256_hex(&bytes),
    };
    fs::write(&metadata_path, serde_json::to_vec_pretty(&metadata)?)?;

    Ok((pdf_path, canonical_url))
}

fn datasheet_download_headers(url: &Url) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36",
        ),
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/pdf,*/*"));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));

    if let Some(host) = url.host_str() {
        let referer = format!("{}://{host}/", url.scheme());
        headers.insert(
            REFERER,
            HeaderValue::from_str(&referer).context("Invalid generated referer header")?,
        );
    }

    Ok(headers)
}

fn fetch_pdf_with_headers(
    client: &Client,
    url: &str,
    headers: HeaderMap,
) -> Result<(Vec<u8>, Option<String>)> {
    let response = client
        .get(url)
        .headers(headers)
        .timeout(std::time::Duration::from_secs(
            DATASHEET_DOWNLOAD_TIMEOUT_SECS,
        ))
        .send()
        .with_context(|| format!("Failed request to {url}"))?;

    if !response.status().is_success() {
        anyhow::bail!(
            "Datasheet download failed with status {}",
            response.status()
        );
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let bytes = response.bytes()?.to_vec();
    Ok((bytes, content_type))
}

fn extract_datasheet_url_from_kicad_sym(path: &Path, symbol_name: Option<&str>) -> Result<String> {
    let symbol_lib = pcb_eda::SymbolLibrary::from_file(path)
        .with_context(|| format!("Failed to parse KiCad symbol file {}", path.display()))?;

    let symbol = select_symbol_from_library(&symbol_lib, path, symbol_name)?;
    symbol
        .datasheet
        .as_deref()
        .map(str::trim)
        .filter(|v| is_usable_datasheet_value(v))
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow::anyhow!("No valid Datasheet URL found in {}", path.display()))
}

fn select_symbol_from_library<'a>(
    symbol_lib: &'a pcb_eda::SymbolLibrary,
    path: &Path,
    symbol_name: Option<&str>,
) -> Result<&'a pcb_eda::Symbol> {
    let symbols = symbol_lib.symbols();
    if symbols.is_empty() {
        anyhow::bail!("No symbols found in {}", path.display());
    }

    if let Some(name) = symbol_name {
        return symbol_lib.get_symbol(name).ok_or_else(|| {
            let available = symbol_lib.symbol_names().join(", ");
            anyhow::anyhow!(
                "Symbol '{}' not found in {}. Available symbols: {}",
                name,
                path.display(),
                available
            )
        });
    }

    if symbols.len() > 1 {
        let available = symbol_lib.symbol_names().join(", ");
        anyhow::bail!(
            "kicad_sym_path contains {} symbols in {}. Provide symbol_name. Available symbols: {}",
            symbols.len(),
            path.display(),
            available
        );
    }

    Ok(&symbols[0])
}

fn is_usable_datasheet_value(value: &str) -> bool {
    if value.is_empty() || value == "~" {
        return false;
    }
    value.starts_with("http://") || value.starts_with("https://")
}

fn validate_local_pdf(path: &Path) -> Result<()> {
    validate_existing_file_with_extension(path, "pdf_path", "pdf")
}

fn validate_local_kicad_sym(path: &Path) -> Result<()> {
    validate_existing_file_with_extension(path, "kicad_sym_path", "kicad_sym")
}

fn validate_existing_file_with_extension(
    path: &Path,
    field_name: &str,
    extension: &str,
) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("{field_name} does not exist: {}", path.display());
    }
    if path.extension().is_none_or(|ext| ext != extension) {
        anyhow::bail!(
            "{field_name} must point to a .{extension} file: {}",
            path.display()
        );
    }
    Ok(())
}

fn optional_trimmed_string(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn optional_path(args: &Value, key: &str) -> Option<PathBuf> {
    args.get(key).and_then(|v| v.as_str()).map(PathBuf::from)
}

fn canonicalize_url(url: &str) -> Result<String> {
    let mut parsed = Url::parse(url).with_context(|| format!("Invalid datasheet_url: {url}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("datasheet_url must use http or https");
    }

    if let Some(host) = parsed.host_str() {
        parsed
            .set_host(Some(&host.to_lowercase()))
            .context("Failed to normalize URL host")?;
    }
    parsed
        .set_scheme(&parsed.scheme().to_lowercase())
        .map_err(|_| anyhow::anyhow!("Failed to normalize URL scheme"))?;
    parsed.set_fragment(None);

    Ok(parsed.to_string())
}

fn inferred_pdf_filename(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_string())
        .unwrap_or_else(|| "datasheet.pdf".to_string())
}

fn build_resolve_response(
    markdown_path: &Path,
    images_dir: &Path,
    pdf_path: &Path,
    datasheet_url: Option<String>,
    sha256: String,
) -> ResolveDatasheetResponse {
    ResolveDatasheetResponse {
        markdown_path: markdown_path.display().to_string(),
        images_dir: images_dir.display().to_string(),
        pdf_path: pdf_path.display().to_string(),
        datasheet_url,
        sha256,
    }
}

fn looks_like_pdf(bytes: &[u8], content_type: Option<&str>) -> bool {
    if bytes.starts_with(b"%PDF") {
        return true;
    }

    content_type
        .map(|ct| ct.to_ascii_lowercase().contains("application/pdf"))
        .unwrap_or(false)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn materialization_id_for_sha(pdf_sha256: &str) -> Result<String> {
    let namespace = Uuid::parse_str(DATASHEET_NAMESPACE_UUID)
        .context("Invalid datasheet namespace UUID constant")?;
    Ok(Uuid::new_v5(&namespace, pdf_sha256.as_bytes()).to_string())
}

fn url_pdf_cache_dir() -> PathBuf {
    cache_base().join("datasheets").join("pdfs")
}

fn materialized_dir(materialization_id: &str) -> PathBuf {
    cache_base()
        .join("datasheets")
        .join("materialized")
        .join(materialization_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_request_requires_exactly_one_input() {
        let both = serde_json::json!({
            "datasheet_url": "https://example.com/a.pdf",
            "pdf_path": "/tmp/a.pdf"
        });
        assert!(parse_resolve_request(Some(&both)).is_err());

        let none = serde_json::json!({});
        assert!(parse_resolve_request(Some(&none)).is_err());
    }

    #[test]
    fn test_canonicalize_url_normalizes_host_and_removes_fragment() {
        let out = canonicalize_url("HTTPS://EXAMPLE.com/a/b.pdf?x=1#fragment").unwrap();
        assert_eq!(out, "https://example.com/a/b.pdf?x=1");
    }

    #[test]
    fn test_materialization_id_is_deterministic() {
        let a = materialization_id_for_sha("abc123").unwrap();
        let b = materialization_id_for_sha("abc123").unwrap();
        let c = materialization_id_for_sha("abc124").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_extract_datasheet_url_from_symbols_uses_first_valid_value() {
        let source = r#"(kicad_symbol_lib
          (version 20211014)
          (generator kicad_symbol_editor)
          (symbol "A"
            (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
            (property "Value" "A" (at 0 0 0) (effects (font (size 1.27 1.27))))
            (property "Datasheet" "~" (at 0 0 0) (effects (font (size 1.27 1.27))))
          )
          (symbol "B"
            (property "Reference" "U" (at 0 0 0) (effects (font (size 1.27 1.27))))
            (property "Value" "B" (at 0 0 0) (effects (font (size 1.27 1.27))))
            (property "Datasheet" "https://example.com/b.pdf" (at 0 0 0) (effects (font (size 1.27 1.27))))
          )
        )"#;

        let lib = pcb_eda::SymbolLibrary::from_string(source, "kicad_sym").unwrap();
        let path = Path::new("multi.kicad_sym");

        let err = select_symbol_from_library(&lib, path, None).unwrap_err();
        assert!(
            err.to_string().contains("Provide symbol_name"),
            "unexpected error: {err}"
        );

        let symbol = select_symbol_from_library(&lib, path, Some("B")).unwrap();
        assert_eq!(symbol.name, "B");
        assert_eq!(
            symbol.datasheet.as_deref(),
            Some("https://example.com/b.pdf")
        );
    }

    #[test]
    fn test_parse_request_rejects_symbol_name_without_kicad_sym_path() {
        let args = serde_json::json!({
            "symbol_name": "ADC121"
        });
        assert!(parse_resolve_request(Some(&args)).is_err());
    }
}
