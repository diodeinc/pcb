use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelAvailability {
    #[serde(rename = "ECAD_model")]
    pub ecad_model: bool,
    #[serde(rename = "STEP_model")]
    pub step_model: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub part_number: String,
    pub description: Option<String>,
    pub package_category: Option<String>,
    pub component_id: String,
    pub datasheets: Vec<String>,
    pub model_availability: ModelAvailability,
}

#[derive(Debug, Clone)]
pub struct SearchOptions {
    pub mpn: String,
}

#[derive(Debug, Clone)]
pub struct DownloadMetadata {
    pub mpn: String,
    pub timestamp: String,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct DownloadResult {
    pub symbol_url: Option<String>,
    pub footprint_url: Option<String>,
    pub step_url: Option<String>,
    pub metadata: DownloadMetadata,
}

#[derive(Serialize)]
struct SearchRequest {
    mpn: String,
}

#[derive(Deserialize)]
struct SearchResponse {
    #[allow(dead_code)]
    part_number: String,
    #[allow(dead_code)]
    description: Option<String>,
    #[allow(dead_code)]
    package_category: Option<String>,
    #[allow(dead_code)]
    component_id: String,
    #[allow(dead_code)]
    datasheets: Vec<String>,
    #[allow(dead_code)]
    model_availability: ModelAvailability,
    #[allow(dead_code)]
    price_stock_link: Option<String>,
}

#[derive(Serialize)]
struct DownloadRequest {
    component_id: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct DownloadResponse {
    #[serde(rename = "symbolUrl")]
    symbol_url: Option<String>,
    #[serde(rename = "footprintUrl")]
    footprint_url: Option<String>,
    #[serde(rename = "stepUrl")]
    step_url: Option<String>,
    metadata: DownloadResponseMetadata,
}

#[derive(Deserialize)]
struct DownloadResponseMetadata {
    mpn: String,
    timestamp: String,
    source: String,
}

pub struct ComponentClient {
    api_base_url: String,
    pub auth_token: String,
    client: Client,
}

impl ComponentClient {
    pub fn new(api_base_url: String, auth_token: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .context("Failed to create HTTP client")?;

        Ok(Self {
            api_base_url,
            auth_token,
            client,
        })
    }

    pub fn search(&self, options: SearchOptions) -> Result<Vec<SearchResult>> {
        let url = format!("{}/api/component/search", self.api_base_url);
        let request_body = SearchRequest { mpn: options.mpn };

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.auth_token)
            .json(&request_body)
            .send()
            .context("Failed to send search request")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().unwrap_or_default();
            anyhow::bail!("Search failed ({}): {}", status, error_text);
        }

        let results: Vec<SearchResponse> = response
            .json()
            .context("Failed to parse search response")?;

        Ok(results
            .into_iter()
            .map(|r| SearchResult {
                part_number: r.part_number,
                description: r.description,
                package_category: r.package_category,
                component_id: r.component_id,
                datasheets: r.datasheets,
                model_availability: r.model_availability,
            })
            .collect())
    }

    pub fn download(&self, component_id: &str) -> Result<DownloadResult> {
        let url = format!("{}/api/component/download", self.api_base_url);
        let request_body = DownloadRequest {
            component_id: component_id.to_string(),
        };

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.auth_token)
            .json(&request_body)
            .send()
            .context("Failed to send download request")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().unwrap_or_default();
            anyhow::bail!("Download failed ({}): {}", status, error_text);
        }

        let download_response: DownloadResponse = response
            .json()
            .context("Failed to parse download response")?;

        Ok(DownloadResult {
            symbol_url: download_response.symbol_url,
            footprint_url: download_response.footprint_url,
            step_url: download_response.step_url,
            metadata: DownloadMetadata {
                mpn: download_response.metadata.mpn,
                timestamp: download_response.metadata.timestamp,
                source: download_response.metadata.source,
            },
        })
    }

    pub fn download_file(&self, url: &str, output_path: &Path) -> Result<()> {
        let response = self
            .client
            .get(url)
            .send()
            .context("Failed to download file")?;

        if !response.status().is_success() {
            let status = response.status();
            anyhow::bail!("File download failed with status: {}", status);
        }

        let content = response.bytes().context("Failed to read response bytes")?;
        std::fs::write(output_path, &content).context("Failed to write file")?;

        Ok(())
    }
}

pub fn decode_component_id(component_id: &str) -> Result<(String, String)> {
    let decoded = general_purpose::STANDARD
        .decode(component_id)
        .context("Failed to decode component ID")?;
    let decoded_str = String::from_utf8(decoded).context("Invalid UTF-8 in component ID")?;

    #[derive(Deserialize)]
    struct ComponentId {
        source: String,
        part_id: String,
    }

    let parsed: ComponentId =
        serde_json::from_str(&decoded_str).context("Failed to parse component ID JSON")?;

    Ok((parsed.source, parsed.part_id))
}
