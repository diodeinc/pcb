use anyhow::{Context, Result};
use rusqlite::auto_extension::{RawAutoExtension, register_auto_extension};

pub mod auth;
mod aws_auth;
pub mod bom;
pub mod component;
pub mod datasheet;
pub mod diode_uri;
mod download_support;
mod endpoint;
pub mod kicad_symbols;
pub mod registry;
pub mod release;
pub mod routing;
pub mod sandbox;
pub mod scan;

pub use auth::{AuthArgs, AuthCommand, AuthTokens, execute as execute_auth, login, logout, status};
pub use bom::{fetch_and_populate_availability, fetch_and_populate_availability_with_context};
pub use component::{
    AddComponentResult, ComponentDownloadResult, ComponentResult, ComponentSearchResult,
    ModelAvailability, SearchArgs, add_component_to_workspace, download_component,
    execute as execute_search, execute_component_from_id, execute_component_from_local_dir,
    execute_web_components_tui, search_components, search_components_with_availability,
};
pub use diode_uri::{DiodeUri, DiodeUriParseError, SandboxFileUri, is_diode_uri};
pub use endpoint::WorkspaceContext;
pub use kicad_symbols::KicadSymbolsClient;
pub use registry::{
    DigikeyClassifications, DigikeyData, DigikeyPriceBreak, ModuleRelations, ParsedQuery,
    RegistryClient, RegistryInfo, RegistryModule, RegistryModuleDependency,
    RegistryModuleEntrypoint, RegistryModuleHit, RegistryModuleSymbol, RegistrySearchClient,
    RegistrySymbol, RegistrySymbolHit, SearchHit,
};
pub use release::{upload_preview, upload_release};
pub use sandbox::{
    ExecSyncOutput, ExecSyncRequest, SandboxClient, SandboxDirEntry, SandboxListResponse,
    SandboxLockGuard, SandboxLockOptions,
};
pub use scan::{ScanArgs, execute as execute_scan};

pub fn get_api_base_url() -> String {
    WorkspaceContext::from_cwd()
        .unwrap_or_default()
        .api_base_url()
        .to_string()
}

pub fn get_web_base_url() -> String {
    WorkspaceContext::from_cwd()
        .unwrap_or_default()
        .web_base_url()
        .to_string()
}

pub(crate) fn ensure_sqlite_vec_registered() -> Result<()> {
    unsafe {
        // SQLite intentionally erases auto-extension entrypoint types to `void(*)(void)`.
        // Let rusqlite define the target-correct callback signature for us.
        let init = std::mem::transmute::<unsafe extern "C" fn(), RawAutoExtension>(
            sqlite_vec::sqlite3_vec_init,
        );
        register_auto_extension(init).context("Failed to register sqlite-vec auto-extension")
    }
}
