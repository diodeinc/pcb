// Use pipe-safe replacements for the standard printing macros in CLI command modules.
#[macro_use(println, eprint, eprintln)]
extern crate anstream;

use anyhow::{Context, Result};
use rusqlite::auto_extension::{RawAutoExtension, register_auto_extension};

pub mod auth;
pub mod bom;
mod cache;
pub mod component;
mod component_api;
pub mod datasheet;
mod download_support;
mod endpoint;
mod git_auth;
pub mod kicad_symbols;
pub mod registry;
pub mod release;
pub mod routing;
pub mod sandbox;
pub mod scan;

pub use auth::{AuthArgs, AuthCommand, AuthTokens, execute as execute_auth, login, logout, status};
pub use bom::{
    BomMatchMode, BomMatchOptions, fetch_and_populate_availability,
    fetch_and_populate_availability_with_context, hydrate_schematic_from_bom,
    match_bom_with_context,
};
pub use component::{SearchArgs, execute as execute_search, execute_component_from_local_dir};
pub use component_api::{ComponentArgs, execute_component};
pub use endpoint::WorkspaceContext;
pub use kicad_symbols::KicadSymbolsClient;
pub use pcb_diode_uri::{DiodeUri, DiodeUriParseError, SandboxFileUri, is_diode_uri};
pub use registry::{
    DigikeyClassifications, DigikeyData, DigikeyPriceBreak, ModuleRelations, ParsedQuery,
    RegistryClient, RegistryInfo, RegistryModule, RegistryModuleDependency,
    RegistryModuleEntrypoint, RegistryModuleHit, RegistryModuleSymbol, RegistrySearchClient,
    RegistrySymbol, RegistrySymbolHit, SearchHit,
};
pub use release::upload_release;
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
