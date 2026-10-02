// Use pipe-safe replacements for the standard printing macros in CLI command modules.
#[macro_use(println, eprint, eprintln)]
extern crate anstream;

pub mod auth;
pub mod bom;
pub mod component;
mod component_api;
pub mod datasheet;
mod download_support;
mod endpoint;
mod git_auth;
pub mod kicad_symbols;
pub mod registry;
pub mod release;
pub mod sandbox;
pub mod scan;

pub use auth::{AuthArgs, AuthCommand, AuthTokens, execute as execute_auth};
pub use bom::match_bom_with_context;
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
