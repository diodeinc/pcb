pub mod auth;
pub mod bom;
pub mod component;
pub mod datasheet;
pub mod mcp;
pub mod registry;
pub mod release;
pub mod routing;
pub mod scan;

pub use auth::{AuthArgs, AuthCommand, AuthTokens, execute as execute_auth, login, logout, status};
pub use bom::fetch_and_populate_availability;
pub use component::{
    AddComponentResult, ComponentDownloadResult, ComponentResult, ComponentSearchResult,
    ModelAvailability, SearchArgs, add_component_to_workspace, download_component,
    execute as execute_search, execute_web_components_tui, search_components,
    search_components_with_availability,
};
pub use registry::{
    DigikeyData, EDatasheetComponentId, EDatasheetData, PackageDependency, PackageRelations,
    ParsedQuery, RegistryClient, RegistryPackage, RegistryPart, SearchHit,
};
pub use release::{upload_preview, upload_release};
pub use scan::{
    ScanArgs, ScanModel, ScanModelArg, ScanOptions, ScanResult, execute as execute_scan,
    scan_from_source_path, scan_pdf, scan_with_defaults,
};

fn get_api_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_API_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3001".to_string();
    #[cfg(not(debug_assertions))]
    return "https://api.diode.computer".to_string();
}

fn get_web_base_url() -> String {
    if let Ok(url) = std::env::var("DIODE_APP_URL") {
        return url;
    }

    #[cfg(debug_assertions)]
    return "http://localhost:3000".to_string();
    #[cfg(not(debug_assertions))]
    return "https://app.diode.computer".to_string();
}
