use anyhow::Result;
use clap::{ArgGroup, Args, Subcommand};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::str::FromStr;
use std::time::Duration;

const COMPONENT_SEARCH_PATH: &str = "/api/v2/components/search";
const COMPONENT_DOWNLOAD_PATH: &str = "/api/v2/components/download";

#[derive(Args, Debug)]
#[command(about = "Access the component API")]
pub struct ComponentArgs {
    #[command(subcommand)]
    command: ComponentCommand,
}

#[derive(Subcommand, Debug)]
enum ComponentCommand {
    /// Search for components
    Search(ComponentApiSearchArgs),

    /// Request component download metadata and signed asset URLs
    Download(ComponentApiDownloadArgs),
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, Default)]
enum ComponentOutputFormat {
    #[default]
    Pretty,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ComponentBackends(Vec<ComponentEdaBackend>);

impl FromStr for ComponentBackends {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        if value == "none" {
            return Ok(Self(Vec::new()));
        }

        let mut backends = Vec::new();
        for value in value.split(',') {
            let backend = match value.trim() {
                "cse" => ComponentEdaBackend::Cse,
                "lcsc" => ComponentEdaBackend::Lcsc,
                "ncti" => ComponentEdaBackend::Ncti,
                "" => return Err("backend list cannot contain an empty value".to_string()),
                value => {
                    return Err(format!(
                        "unknown backend '{value}'; expected cse, lcsc, ncti, or none"
                    ));
                }
            };

            if backends.contains(&backend) {
                return Err(format!("backend '{value}' is listed more than once"));
            }
            backends.push(backend);
        }

        Ok(Self(backends))
    }
}

#[derive(Args, Debug)]
struct ComponentApiSearchArgs {
    /// Search query (MPN, description, or keywords)
    query: String,

    /// EDA backends as a comma-separated list, or "none"; omit for server defaults
    #[arg(long, value_name = "BACKENDS")]
    backends: Option<ComponentBackends>,

    /// Maximum number of rows to return
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=100))]
    limit: Option<u8>,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = ComponentOutputFormat::Pretty)]
    format: ComponentOutputFormat,
}

#[derive(Args, Debug)]
#[command(group(
    ArgGroup::new("provider")
        .required(true)
        .multiple(false)
        .args(["cse_part_ref", "lcsc_part_number", "ncti_component_id"])
))]
struct ComponentApiDownloadArgs {
    /// Canonical manufacturer part number
    #[arg(long)]
    mpn: String,

    /// Canonical manufacturer name
    #[arg(long)]
    manufacturer: String,

    /// CSE part reference returned by component search
    #[arg(long, value_name = "REF")]
    cse_part_ref: Option<String>,

    /// LCSC part number returned by component search
    #[arg(long, value_name = "PART_NUMBER")]
    lcsc_part_number: Option<String>,

    /// NCTI component ID returned by component search
    #[arg(long, value_name = "ID")]
    ncti_component_id: Option<String>,

    /// Output format
    #[arg(short = 'f', long, value_enum, default_value_t = ComponentOutputFormat::Pretty)]
    format: ComponentOutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ComponentEdaBackend {
    Cse,
    Lcsc,
    Ncti,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentEdaSearchResult {
    pub description: Option<String>,
    pub category: Option<String>,
    pub package: Option<String>,
    pub symbol: bool,
    pub footprint: bool,
    pub step: bool,
    pub datasheet_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CseComponentSearchResult {
    #[serde(flatten)]
    pub result: ComponentEdaSearchResult,
    pub part_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LcscComponentSearchResult {
    #[serde(flatten)]
    pub result: ComponentEdaSearchResult,
    pub part_number: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NctiComponentSearchResult {
    #[serde(flatten)]
    pub result: ComponentEdaSearchResult,
    pub component_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DigiKeyComponentSearchResult {
    pub product_number: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub package: Option<String>,
    pub datasheet_url: Option<String>,
    pub product_url: Option<String>,
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSearchResult {
    pub mpn: String,
    pub manufacturer: String,
    pub cse: Option<CseComponentSearchResult>,
    pub lcsc: Option<LcscComponentSearchResult>,
    pub ncti: Option<NctiComponentSearchResult>,
    pub digikey: Option<DigiKeyComponentSearchResult>,
    #[serde(default)]
    pub offers: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComponentSearchRequest {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backends: Option<Vec<ComponentEdaBackend>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CseComponentReference {
    pub part_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LcscComponentReference {
    pub part_number: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NctiComponentReference {
    pub component_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ComponentDownloadRequest {
    Cse {
        mpn: String,
        manufacturer: String,
        cse: CseComponentReference,
    },
    Lcsc {
        mpn: String,
        manufacturer: String,
        lcsc: LcscComponentReference,
    },
    Ncti {
        mpn: String,
        manufacturer: String,
        ncti: NctiComponentReference,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedComponentAssets {
    pub symbol_url: String,
    pub footprint_url: Option<String>,
    pub step_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedCseComponent {
    #[serde(flatten)]
    pub assets: DownloadedComponentAssets,
    pub part_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedLcscComponent {
    #[serde(flatten)]
    pub assets: DownloadedComponentAssets,
    pub part_number: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedNctiComponent {
    #[serde(flatten)]
    pub assets: DownloadedComponentAssets,
    pub component_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ComponentDownloadResult {
    Cse {
        mpn: String,
        manufacturer: String,
        cse: DownloadedCseComponent,
    },
    Lcsc {
        mpn: String,
        manufacturer: String,
        lcsc: DownloadedLcscComponent,
    },
    Ncti {
        mpn: String,
        manufacturer: String,
        ncti: DownloadedNctiComponent,
    },
}

struct ComponentApiResponse {
    status: reqwest::StatusCode,
    body: String,
}

fn post_component_api<T: Serialize>(
    auth_token: Option<&str>,
    path: &str,
    request: &T,
) -> Result<ComponentApiResponse> {
    let api_base_url = crate::get_api_base_url();
    let url = format!("{api_base_url}{path}");
    let client = Client::builder().timeout(Duration::from_secs(60)).build()?;
    let response = crate::auth::apply_bearer_auth(client.post(&url), auth_token)
        .json(request)
        .send()?;
    let status = response.status();
    let body = response.text()?;

    Ok(ComponentApiResponse { status, body })
}

pub fn execute_component(args: ComponentArgs) -> Result<()> {
    let auth_token = crate::auth::get_api_token()?;

    match args.command {
        ComponentCommand::Search(args) => {
            let request = ComponentSearchRequest {
                query: args.query,
                backends: args.backends.map(|backends| backends.0),
                limit: args.limit,
            };
            let response =
                post_component_api(auth_token.as_deref(), COMPONENT_SEARCH_PATH, &request)?;
            output_component_response::<Vec<ComponentSearchResult>>(
                response,
                args.format,
                |results| print_component_search_results(results),
            )
        }
        ComponentCommand::Download(args) => {
            let request = if let Some(part_ref) = args.cse_part_ref {
                ComponentDownloadRequest::Cse {
                    mpn: args.mpn,
                    manufacturer: args.manufacturer,
                    cse: CseComponentReference { part_ref },
                }
            } else if let Some(part_number) = args.lcsc_part_number {
                ComponentDownloadRequest::Lcsc {
                    mpn: args.mpn,
                    manufacturer: args.manufacturer,
                    lcsc: LcscComponentReference { part_number },
                }
            } else if let Some(component_id) = args.ncti_component_id {
                ComponentDownloadRequest::Ncti {
                    mpn: args.mpn,
                    manufacturer: args.manufacturer,
                    ncti: NctiComponentReference { component_id },
                }
            } else {
                unreachable!("clap requires exactly one component provider");
            };
            let response =
                post_component_api(auth_token.as_deref(), COMPONENT_DOWNLOAD_PATH, &request)?;
            output_component_response(response, args.format, print_component_download_result)
        }
    }
}

fn output_component_response<T: for<'de> Deserialize<'de>>(
    response: ComponentApiResponse,
    format: ComponentOutputFormat,
    print_pretty: impl FnOnce(&T),
) -> Result<()> {
    if matches!(format, ComponentOutputFormat::Json) {
        pcb_ui::write_stdout(|stdout| write_component_json_response(stdout, &response.body))?;
        if response.status.is_success() {
            return Ok(());
        }
        anyhow::bail!("Component API request failed with HTTP {}", response.status);
    }

    if !response.status.is_success() {
        anyhow::bail!(
            "Component API request failed ({}): {}",
            response.status,
            response.body
        );
    }

    let result = serde_json::from_str(&response.body)?;
    print_pretty(&result);
    Ok(())
}

fn write_component_json_response(writer: &mut (impl Write + ?Sized), body: &str) -> io::Result<()> {
    writer.write_all(body.as_bytes())?;
    writer.flush()
}

fn print_component_search_results(results: &[ComponentSearchResult]) {
    if results.is_empty() {
        println!("No components found.");
        return;
    }

    for (index, result) in results.iter().enumerate() {
        if index > 0 {
            println!();
        }

        println!("{} — {}", result.mpn, result.manufacturer);
        if let Some(provider) = &result.cse {
            print_eda_provider("cse", &provider.part_ref, &provider.result);
        }
        if let Some(provider) = &result.lcsc {
            print_eda_provider("lcsc", &provider.part_number, &provider.result);
        }
        if let Some(provider) = &result.ncti {
            print_eda_provider("ncti", &provider.component_id, &provider.result);
        }
        if let Some(provider) = &result.digikey {
            println!("  digikey: {}", provider.product_number);
            print_optional_field("description", provider.description.as_deref(), 4);
            print_optional_field("category", provider.category.as_deref(), 4);
            print_optional_field("package", provider.package.as_deref(), 4);
            print_optional_field("datasheet", provider.datasheet_url.as_deref(), 4);
            print_optional_field("product", provider.product_url.as_deref(), 4);
            print_optional_field("image", provider.image_url.as_deref(), 4);
        }
        println!("  offers: {}", result.offers.len());
    }
}

fn print_eda_provider(name: &str, reference: &str, result: &ComponentEdaSearchResult) {
    println!("  {name}: {reference}");
    print_optional_field("description", result.description.as_deref(), 4);
    print_optional_field("category", result.category.as_deref(), 4);
    print_optional_field("package", result.package.as_deref(), 4);
    println!(
        "    assets: symbol={} footprint={} step={}",
        yes_no(result.symbol),
        yes_no(result.footprint),
        yes_no(result.step)
    );
    print_optional_field("datasheet", result.datasheet_url.as_deref(), 4);
}

fn print_component_download_result(result: &ComponentDownloadResult) {
    let (mpn, manufacturer, provider, reference, assets) = match result {
        ComponentDownloadResult::Cse {
            mpn,
            manufacturer,
            cse,
        } => (
            mpn.as_str(),
            manufacturer.as_str(),
            "cse",
            cse.part_ref.as_str(),
            &cse.assets,
        ),
        ComponentDownloadResult::Lcsc {
            mpn,
            manufacturer,
            lcsc,
        } => (
            mpn.as_str(),
            manufacturer.as_str(),
            "lcsc",
            lcsc.part_number.as_str(),
            &lcsc.assets,
        ),
        ComponentDownloadResult::Ncti {
            mpn,
            manufacturer,
            ncti,
        } => (
            mpn.as_str(),
            manufacturer.as_str(),
            "ncti",
            ncti.component_id.as_str(),
            &ncti.assets,
        ),
    };

    println!("{mpn} — {manufacturer}");
    println!("  provider: {provider}");
    println!("  reference: {reference}");
    println!("  symbol: {}", assets.symbol_url);
    println!(
        "  footprint: {}",
        assets.footprint_url.as_deref().unwrap_or("—")
    );
    println!("  step: {}", assets.step_url.as_deref().unwrap_or("—"));
}

fn print_optional_field(name: &str, value: Option<&str>, indent: usize) {
    if let Some(value) = value {
        println!("{:indent$}{name}: {value}", "");
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(clap::Parser)]
    struct TestCli {
        #[command(subcommand)]
        command: TestCommand,
    }

    #[derive(clap::Subcommand)]
    enum TestCommand {
        Component(ComponentArgs),
    }

    #[test]
    fn search_uses_one_comma_separated_backends_option() {
        let parsed = TestCli::try_parse_from([
            "pcb",
            "component",
            "search",
            "TPS54331DR",
            "--backends",
            "cse,lcsc",
            "--limit",
            "10",
            "--format",
            "json",
        ])
        .unwrap();

        let TestCommand::Component(component) = parsed.command;
        let ComponentCommand::Search(search) = component.command else {
            panic!("expected component search");
        };
        assert_eq!(
            search.backends.unwrap().0,
            vec![ComponentEdaBackend::Cse, ComponentEdaBackend::Lcsc]
        );
        assert_eq!(search.limit, Some(10));
        assert!(matches!(search.format, ComponentOutputFormat::Json));

        assert!(
            TestCli::try_parse_from([
                "pcb",
                "component",
                "search",
                "TPS54331DR",
                "--backends",
                "cse",
                "--backends",
                "lcsc",
            ])
            .is_err()
        );
    }

    #[test]
    fn backend_selection_maps_directly_to_the_api_field() {
        assert!(ComponentBackends::from_str("none").unwrap().0.is_empty());
        assert_eq!(
            ComponentBackends::from_str("cse,ncti").unwrap().0,
            vec![ComponentEdaBackend::Cse, ComponentEdaBackend::Ncti]
        );
        assert!(ComponentBackends::from_str("none,cse").is_err());
        assert!(ComponentBackends::from_str("cse,cse").is_err());
        assert!(ComponentBackends::from_str("digikey").is_err());

        let omitted = serde_json::to_value(ComponentSearchRequest {
            query: "TPS54331DR".to_string(),
            backends: None,
            limit: None,
        })
        .unwrap();
        assert_eq!(omitted, serde_json::json!({"query": "TPS54331DR"}));

        let none = serde_json::to_value(ComponentSearchRequest {
            query: "TPS54331DR".to_string(),
            backends: Some(Vec::new()),
            limit: Some(5),
        })
        .unwrap();
        assert_eq!(
            none,
            serde_json::json!({
                "query": "TPS54331DR",
                "backends": [],
                "limit": 5
            })
        );
    }

    #[test]
    fn download_requires_exactly_one_provider_reference() {
        let valid = TestCli::try_parse_from([
            "pcb",
            "component",
            "download",
            "--mpn",
            "TPS54331DR",
            "--manufacturer",
            "Texas Instruments",
            "--cse-part-ref",
            "TPS54331DR/Texas%20Instruments",
        ]);
        assert!(valid.is_ok());

        let missing = TestCli::try_parse_from([
            "pcb",
            "component",
            "download",
            "--mpn",
            "TPS54331DR",
            "--manufacturer",
            "Texas Instruments",
        ]);
        assert!(missing.is_err());

        let multiple = TestCli::try_parse_from([
            "pcb",
            "component",
            "download",
            "--mpn",
            "TPS54331DR",
            "--manufacturer",
            "Texas Instruments",
            "--cse-part-ref",
            "cse-ref",
            "--lcsc-part-number",
            "C9865",
        ]);
        assert!(multiple.is_err());
    }

    #[test]
    fn download_request_contains_one_explicit_provider() {
        let request = ComponentDownloadRequest::Lcsc {
            mpn: "TPS54331DR".to_string(),
            manufacturer: "Texas Instruments".to_string(),
            lcsc: LcscComponentReference {
                part_number: "C9865".to_string(),
            },
        };

        assert_eq!(
            serde_json::to_value(request).unwrap(),
            serde_json::json!({
                "mpn": "TPS54331DR",
                "manufacturer": "Texas Instruments",
                "lcsc": {"part_number": "C9865"}
            })
        );
    }

    #[test]
    fn json_output_preserves_the_api_body() {
        let body = r#"[{"unknown":true,"nullable":null}]"#;
        let mut output = Vec::new();
        write_component_json_response(&mut output, body).unwrap();
        assert_eq!(output, body.as_bytes());
    }

    #[test]
    fn response_types_accept_provider_sections_and_unknown_fields() {
        let search: Vec<ComponentSearchResult> = serde_json::from_value(serde_json::json!([{
            "mpn": "TPS54331DR",
            "manufacturer": "Texas Instruments",
            "cse": {
                "part_ref": "TPS54331DR/Texas%20Instruments",
                "description": "Buck regulator",
                "category": "Power Management",
                "package": "SOIC-8",
                "symbol": true,
                "footprint": true,
                "step": true,
                "datasheet_url": null
            },
            "lcsc": null,
            "ncti": null,
            "digikey": null,
            "offers": [],
            "unknown": true
        }]))
        .unwrap();
        assert_eq!(search[0].mpn, "TPS54331DR");

        let download: ComponentDownloadResult = serde_json::from_value(serde_json::json!({
            "mpn": "TPS54331DR",
            "manufacturer": "Texas Instruments",
            "ncti": {
                "component_id": "ncti-123",
                "symbol_url": "https://example.com/symbol",
                "footprint_url": null,
                "step_url": null
            },
            "unknown": true
        }))
        .unwrap();
        assert!(matches!(download, ComponentDownloadResult::Ncti { .. }));
    }
}
