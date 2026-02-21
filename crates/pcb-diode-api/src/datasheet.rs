use anyhow::{Context, Result};
use atomicwrites::{AtomicFile, OverwriteBehavior};
use pcb_zen::cache_index::cache_base;
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use url::Url;
use uuid::Uuid;

use crate::scan::{
    calculate_sha256, download_file, extract_zip, request_process, request_upload_url, upload_pdf,
};

const DATASHEET_NAMESPACE_UUID: &str = "fe255507-b3f4-4ec0-98cb-9e3f90cfd8eb";

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
}

#[derive(Debug, Clone)]
enum ProcessSource {
    SourceUrl(String),
    LocalPdf { path: PathBuf, sha256: String },
}

#[derive(Debug, Clone)]
struct ResolveExecution {
    process_source: ProcessSource,
    materialization_key: String,
    pdf_path: PathBuf,
    datasheet_url: Option<String>,
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

    let execution = match input {
        ResolveDatasheetInput::DatasheetUrl(url) => {
            let canonical_url = canonicalize_url(url)?;
            build_source_url_execution(canonical_url)
        }
        ResolveDatasheetInput::PdfPath(path) => build_local_pdf_execution(path.clone())?,
        ResolveDatasheetInput::KicadSymPath { path, symbol_name } => {
            let url = extract_datasheet_url_from_kicad_sym(path, symbol_name.as_deref())?;
            let canonical_url = canonicalize_url(&url)?;
            build_source_url_execution(canonical_url)
        }
    };

    execute_resolve_execution(&client, auth_token, execution)
}

fn build_local_pdf_execution(pdf_path: PathBuf) -> Result<ResolveExecution> {
    let pdf_sha256 = calculate_sha256(&pdf_path)?;

    Ok(ResolveExecution {
        process_source: ProcessSource::LocalPdf {
            path: pdf_path.clone(),
            sha256: pdf_sha256.clone(),
        },
        materialization_key: pdf_sha256,
        pdf_path,
        datasheet_url: None,
    })
}

fn build_source_url_execution(canonical_url: String) -> ResolveExecution {
    ResolveExecution {
        process_source: ProcessSource::SourceUrl(canonical_url.clone()),
        materialization_key: format!("url:{canonical_url}"),
        pdf_path: url_pdf_cache_path(&canonical_url),
        datasheet_url: Some(canonical_url),
    }
}

fn execute_resolve_execution(
    client: &Client,
    auth_token: &str,
    execution: ResolveExecution,
) -> Result<ResolveDatasheetResponse> {
    let api_base_url = crate::get_api_base_url();
    let materialization_id = materialization_id_for_key(&execution.materialization_key)?;
    let materialized_dir = materialized_dir(&materialization_id);
    let markdown_path = materialized_dir.join("datasheet.md");
    let images_dir = materialized_dir.join("images");
    let complete_marker = materialized_dir.join(".complete");

    if is_valid_materialized_cache(&markdown_path, &images_dir, &complete_marker)?
        && is_valid_cached_pdf(&execution.pdf_path)?
    {
        return Ok(build_resolve_response(
            &markdown_path,
            &images_dir,
            &execution.pdf_path,
            execution.datasheet_url,
        ));
    }
    reset_materialized_cache(&markdown_path, &images_dir, &complete_marker);

    if let Some(parent) = execution.pdf_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let (source_path, source_url, refresh_pdf_from_process) =
        resolve_process_source(client, auth_token, &api_base_url, execution.process_source)?;

    let process = request_process(
        client,
        auth_token,
        &api_base_url,
        source_path.as_deref(),
        source_url.as_deref(),
        None,
    )?;

    if refresh_pdf_from_process {
        let source_pdf_url = process
            .source_pdf_url
            .as_deref()
            .context("Scan API did not return sourcePdfUrl for URL input")?;
        download_file(client, source_pdf_url, &execution.pdf_path)
            .context("Failed to download source PDF output")?;
    }

    materialize_process_outputs(
        client,
        &process,
        &materialized_dir,
        &markdown_path,
        &images_dir,
        &complete_marker,
    )?;

    Ok(build_resolve_response(
        &markdown_path,
        &images_dir,
        &execution.pdf_path,
        execution.datasheet_url,
    ))
}

fn resolve_process_source(
    client: &Client,
    auth_token: &str,
    api_base_url: &str,
    source: ProcessSource,
) -> Result<(Option<String>, Option<String>, bool)> {
    match source {
        ProcessSource::SourceUrl(url) => Ok((None, Some(url), true)),
        ProcessSource::LocalPdf { path, sha256 } => {
            let filename = inferred_pdf_filename(&path);
            let upload = request_upload_url(client, auth_token, api_base_url, &sha256, &filename)?;
            if let Some(upload_url) = upload.upload_url.as_deref() {
                upload_pdf(client, upload_url, &path)?;
            }
            Ok((Some(upload.source_path), None, false))
        }
    }
}

fn materialize_process_outputs(
    client: &Client,
    process: &crate::scan::ProcessResponse,
    materialized_dir: &Path,
    markdown_path: &Path,
    images_dir: &Path,
    complete_marker: &Path,
) -> Result<()> {
    fs::create_dir_all(materialized_dir)?;
    let _ = fs::remove_file(complete_marker);

    download_file(client, &process.markdown_url, markdown_path)
        .context("Failed to download markdown output")?;

    if let Some(images_zip_url) = process.images_zip_url.as_deref() {
        let zip_path = materialized_dir.join("images.zip");
        let temp_images_dir = materialized_dir.join(format!(".images-{}", Uuid::new_v4()));

        let extract_result = (|| -> Result<()> {
            download_file(client, images_zip_url, &zip_path)
                .context("Failed to download image archive")?;
            fs::create_dir_all(&temp_images_dir)?;
            extract_zip(&zip_path, &temp_images_dir)?;

            if images_dir.exists() {
                fs::remove_dir_all(images_dir)?;
            }
            fs::rename(&temp_images_dir, images_dir)?;
            Ok(())
        })();

        let _ = fs::remove_file(&zip_path);
        if extract_result.is_err() {
            let _ = fs::remove_dir_all(&temp_images_dir);
        }
        extract_result?;
    } else {
        if images_dir.exists() {
            fs::remove_dir_all(images_dir)?;
        }
        fs::create_dir_all(images_dir)?;
    }

    write_complete_marker(complete_marker)?;
    Ok(())
}

fn write_complete_marker(path: &Path) -> Result<()> {
    AtomicFile::new(path, OverwriteBehavior::AllowOverwrite)
        .write(|f| {
            f.write_all(b"ok")?;
            f.flush()
        })
        .map_err(|err| anyhow::anyhow!("Failed to finalize datasheet cache: {err}"))?;
    Ok(())
}

fn is_valid_materialized_cache(
    markdown_path: &Path,
    images_dir: &Path,
    complete_marker: &Path,
) -> Result<bool> {
    Ok(is_non_empty_file(markdown_path)?
        && images_dir.is_dir()
        && is_non_empty_file(complete_marker)?)
}

fn reset_materialized_cache(markdown_path: &Path, images_dir: &Path, complete_marker: &Path) {
    if complete_marker.exists() {
        let _ = fs::remove_file(complete_marker);
    }
    if markdown_path.exists() {
        let _ = fs::remove_file(markdown_path);
    }
    if images_dir.exists() {
        let _ = fs::remove_dir_all(images_dir);
    }
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
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
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
) -> ResolveDatasheetResponse {
    ResolveDatasheetResponse {
        markdown_path: markdown_path.display().to_string(),
        images_dir: images_dir.display().to_string(),
        pdf_path: pdf_path.display().to_string(),
        datasheet_url,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn materialization_id_for_key(key: &str) -> Result<String> {
    let namespace = Uuid::parse_str(DATASHEET_NAMESPACE_UUID)
        .context("Invalid datasheet namespace UUID constant")?;
    Ok(Uuid::new_v5(&namespace, key.as_bytes()).to_string())
}

fn materialized_dir(materialization_id: &str) -> PathBuf {
    cache_base()
        .join("datasheets")
        .join("materialized")
        .join(materialization_id)
}

fn url_pdf_cache_dir() -> PathBuf {
    cache_base().join("datasheets").join("pdfs")
}

fn url_pdf_cache_path(canonical_url: &str) -> PathBuf {
    let key = sha256_hex(canonical_url.as_bytes());
    url_pdf_cache_dir().join(format!("{key}.pdf"))
}

fn is_non_empty_file(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() > 0),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn is_valid_cached_pdf(path: &Path) -> Result<bool> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    let mut header = [0u8; 4];
    let read = file.read(&mut header)?;
    Ok(read == 4 && header == *b"%PDF")
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
        let a = materialization_id_for_key("abc123").unwrap();
        let b = materialization_id_for_key("abc123").unwrap();
        let c = materialization_id_for_key("abc124").unwrap();
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

    #[test]
    fn test_parse_request_trims_pdf_path() {
        let path = std::env::temp_dir().join(format!("datasheet-test-{}.pdf", Uuid::new_v4()));
        fs::write(&path, b"%PDF-1.7\n").unwrap();

        let args = serde_json::json!({
            "pdf_path": format!("  {}  ", path.display())
        });
        let parsed = parse_resolve_request(Some(&args)).unwrap();

        match parsed {
            ResolveDatasheetInput::PdfPath(parsed_path) => assert_eq!(parsed_path, path),
            other => panic!("expected PdfPath, got {other:?}"),
        }

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn source_url_maps_to_process_url_field() {
        let client = Client::builder().build().unwrap();

        let (path, url, refresh) = resolve_process_source(
            &client,
            "token",
            "http://localhost:3001",
            ProcessSource::SourceUrl("https://example.com/file.pdf".to_string()),
        )
        .unwrap();
        assert_eq!(path, None);
        assert_eq!(url.as_deref(), Some("https://example.com/file.pdf"));
        assert!(refresh);
    }

    #[test]
    fn local_pdf_execution_uses_local_source() {
        let path = std::env::temp_dir().join(format!("datasheet-local-{}.pdf", Uuid::new_v4()));
        fs::write(&path, b"%PDF-1.7\n").unwrap();

        let execution = build_local_pdf_execution(path.clone()).unwrap();
        assert_eq!(execution.pdf_path, path);
        assert!(execution.datasheet_url.is_none());

        match execution.process_source {
            ProcessSource::LocalPdf {
                path: source_path, ..
            } => assert_eq!(source_path, path),
            _ => panic!("expected LocalPdf process source"),
        }

        fs::remove_file(path).unwrap();
    }

    #[test]
    fn cached_pdf_requires_pdf_magic_header() {
        let good = std::env::temp_dir().join(format!("datasheet-good-{}.pdf", Uuid::new_v4()));
        let bad = std::env::temp_dir().join(format!("datasheet-bad-{}.pdf", Uuid::new_v4()));
        fs::write(&good, b"%PDF-1.7\n").unwrap();
        fs::write(&bad, b"not a pdf").unwrap();

        assert!(is_valid_cached_pdf(&good).unwrap());
        assert!(!is_valid_cached_pdf(&bad).unwrap());

        fs::remove_file(good).unwrap();
        fs::remove_file(bad).unwrap();
    }

    #[test]
    fn response_excludes_legacy_fields() {
        let response = ResolveDatasheetResponse {
            markdown_path: "/tmp/datasheet.md".to_string(),
            images_dir: "/tmp/images".to_string(),
            pdf_path: "/tmp/datasheet.pdf".to_string(),
            datasheet_url: Some("https://example.com/datasheet.pdf".to_string()),
        };

        let value = serde_json::to_value(response).unwrap();
        assert!(value.get("markdown_path").is_some());
        assert!(value.get("images_dir").is_some());
        assert!(value.get("pdf_path").is_some());
        assert!(value.get("datasheet_url").is_some());
        assert!(value.get("sha256").is_none());
        assert!(value.get("source_pdf_url").is_none());
    }
}
