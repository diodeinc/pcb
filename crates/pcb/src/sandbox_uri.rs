#[cfg(feature = "api")]
use anyhow::Context;
use anyhow::{Result, bail};
use std::path::Path;

#[cfg(feature = "api")]
pub use pcb_diode_api::SandboxFileUri;

#[cfg(feature = "api")]
pub fn parse_sandbox_file_arg(path: &Path) -> Result<Option<SandboxFileUri>> {
    let Some(input) = path.to_str() else {
        return Ok(None);
    };
    if !pcb_diode_api::is_diode_uri(input) {
        return Ok(None);
    }

    let uri = pcb_diode_api::SandboxFileUri::parse(input)
        .with_context(|| format!("Invalid remote sandbox URI: {input}"))?;
    Ok(Some(uri))
}

#[cfg(not(feature = "api"))]
pub fn reject_if_diode_uri(path: &Path) -> Result<()> {
    let Some(input) = path.to_str() else {
        return Ok(());
    };
    if input
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("diode://"))
    {
        bail!("Remote sandbox URIs require pcb to be built with the `api` feature");
    }
    Ok(())
}

#[cfg(feature = "api")]
pub fn require_remote_zen_file(uri: &SandboxFileUri) -> Result<()> {
    if !is_zen_path(Path::new(&uri.sandbox_path)) {
        bail!("Expected a .zen file URI, got: {}", uri.sandbox_path);
    }
    Ok(())
}

#[cfg(feature = "api")]
pub fn require_remote_openable_file(uri: &SandboxFileUri) -> Result<()> {
    let path = Path::new(&uri.sandbox_path);
    if is_zen_path(path) || is_kicad_pcb_path(path) {
        return Ok(());
    }
    bail!(
        "Expected a .zen or .kicad_pcb file URI, got: {}",
        uri.sandbox_path
    );
}

#[cfg(feature = "api")]
pub fn is_remote_kicad_pcb_file(uri: &SandboxFileUri) -> bool {
    is_kicad_pcb_path(Path::new(&uri.sandbox_path))
}

pub fn is_kicad_pcb_path(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "kicad_pcb")
}

#[cfg(feature = "api")]
fn is_zen_path(path: &Path) -> bool {
    pcb_zen::file_extensions::is_starlark_file(path.extension())
}
