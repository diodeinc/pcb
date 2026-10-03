use anyhow::Context;
use anyhow::{Result, bail};
use std::path::Path;

pub use pcb_diode_api::SandboxFileUri;

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

pub fn require_remote_zen_file(uri: &SandboxFileUri) -> Result<()> {
    if !is_zen_path(Path::new(&uri.sandbox_path)) {
        bail!("Expected a .zen file URI, got: {}", uri.sandbox_path);
    }
    Ok(())
}

pub fn require_remote_openable_file(uri: &SandboxFileUri) -> Result<()> {
    let path = Path::new(&uri.sandbox_path);
    if is_zen_path(path) || is_kicad_pcb_path(path) || is_kicad_sch_path(path) {
        return Ok(());
    }
    bail!(
        "Expected a .zen, .kicad_pcb or .kicad_sch file URI, got: {}",
        uri.sandbox_path
    );
}

pub fn is_remote_kicad_pcb_file(uri: &SandboxFileUri) -> bool {
    is_kicad_pcb_path(Path::new(&uri.sandbox_path))
}

pub fn is_kicad_pcb_path(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "kicad_pcb")
}

pub fn is_kicad_sch_path(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "kicad_sch")
}

fn is_zen_path(path: &Path) -> bool {
    pcb_zen::file_extensions::is_starlark_file(path.extension())
}
