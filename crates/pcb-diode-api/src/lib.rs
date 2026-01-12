pub mod auth;
pub mod bom;
pub mod component;
pub mod mcp;
pub mod registry;
pub mod routing;
pub mod scan;

pub use auth::{execute as execute_auth, login, logout, status, AuthArgs, AuthCommand, AuthTokens};
pub use bom::{
    fetch_and_populate_availability, BomLine, ComponentOffer, MatchBomResponse, PriceBreak,
};
pub use component::{
    add_component_to_workspace, download_component, execute as execute_search,
    execute_new_component_tui, search_and_add_single, search_components, AddComponentResult,
    ComponentDownloadResult, ComponentSearchResult, ModelAvailability, SearchArgs,
};
pub use registry::{
    DigikeyData, EDatasheetComponentId, EDatasheetData, PackageDependency, PackageRelations,
    ParsedQuery, RegistryClient, RegistryPackage, RegistryPart, SearchHit,
};
pub use scan::{
    execute as execute_scan, scan_from_source_path, scan_pdf, scan_with_defaults, ScanArgs,
    ScanModel, ScanModelArg, ScanOptions, ScanResult,
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
