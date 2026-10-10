// Use pipe-safe replacements for the standard printing macros in CLI command modules.
#[macro_use(println, eprintln)]
extern crate anstream;

pub mod auth;
pub mod bom;
pub mod datasheet;
mod endpoint;
mod git_auth;
pub mod release;
pub mod sandbox;
pub mod scan;

pub use auth::{AuthArgs, AuthCommand, AuthTokens, execute as execute_auth};
pub use bom::match_bom_with_context;
pub use endpoint::WorkspaceContext;
pub use pcb_diode_uri::{DiodeUri, DiodeUriParseError, SandboxFileUri, is_diode_uri};
pub use release::upload_release;
pub use sandbox::{
    ExecSyncOutput, ExecSyncRequest, SandboxClient, SandboxDirEntry, SandboxListResponse,
    SandboxLockGuard, SandboxLockOptions,
};
pub use scan::{ScanArgs, execute as execute_scan};
