use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use clap::Args;
use pcb_layout::utils;
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use tiny_http::{Header, Method, Response, Server};
use uuid::Uuid;

const DFM_REPORT_SUFFIX: &str = ".dfm.json";
const DFM_VIEWER_URL: &str = "https://dfm.diode.computer/";
const MAX_DFM_REPORT_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DFM_URL_BYTES: usize = 900 * 1024;
const DFM_BRIDGE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Args, Debug)]
pub struct OpenArgs {
    /// Path to .zen/.kicad_pcb/.dfm.json file or diode:// sandbox URI
    #[arg(value_name = "FILE", value_hint = clap::ValueHint::FilePath)]
    pub file: PathBuf,

    /// Disable network access (offline mode) - only use vendored dependencies
    #[arg(long = "offline")]
    pub offline: bool,
}

pub fn execute(args: OpenArgs) -> Result<()> {
    if let Some(uri) = crate::sandbox_uri::parse_sandbox_file_arg(&args.file)? {
        crate::sandbox_uri::require_remote_openable_file(&uri)?;
        return crate::remote_sandbox::execute_open(uri, args);
    }

    if is_dfm_report_path(&args.file) {
        return open_dfm_report(&args.file);
    }

    if crate::sandbox_uri::is_kicad_pcb_path(&args.file) {
        return open_pcb_file(&args.file);
    }

    crate::file_walker::require_zen_file(&args.file)?;
    let resolution_result = crate::resolve::resolve(Some(&args.file), args.offline)?;
    let zen_path = &args.file;
    let file_name = zen_path.file_name().unwrap().to_string_lossy();
    let eval_result = pcb_zen::eval(zen_path, resolution_result, Default::default());

    let Some(output) = eval_result.output else {
        anyhow::bail!("Build failed for {}", file_name);
    };
    let Some(schematic) = output.to_schematic_with_diagnostics().output else {
        anyhow::bail!("Build failed for {}", file_name);
    };
    let layout_dir = utils::resolve_layout_dir(&schematic)?
        .ok_or_else(|| anyhow::anyhow!("No layout path defined in {}", file_name))?;
    let kicad_files = utils::require_kicad_files(&layout_dir)?;
    let layout_path = kicad_files.kicad_pcb();
    if !layout_path.exists() {
        anyhow::bail!(
            "Layout file not found: {}. Run 'pcb layout {}' to generate it.",
            layout_path.display(),
            zen_path.display()
        );
    }

    open_pcb_file(&layout_path)
}

pub(crate) fn is_dfm_report_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(DFM_REPORT_SUFFIX))
}

pub(crate) fn open_dfm_report(path: &Path) -> Result<()> {
    let bridge = DfmBridge::new(path)?;
    let url = bridge.url();
    let result = (|| {
        open::that(&url).context("failed to launch the default browser")?;
        bridge.serve(DFM_BRIDGE_TIMEOUT)
    })();
    result.with_context(|| {
        format!(
            "Open {DFM_VIEWER_URL} and select {} manually",
            path.display()
        )
    })
}

struct DfmBridge {
    server: Server,
    root: String,
    page: Vec<u8>,
}

impl DfmBridge {
    fn new(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("failed to inspect DFM report {}", path.display()))?;
        ensure!(
            metadata.is_file(),
            "DFM report is not a regular file: {}",
            path.display()
        );
        ensure!(
            metadata.len() <= MAX_DFM_REPORT_BYTES,
            "DFM report exceeds the 16 MiB automatic-open limit: {}",
            path.display()
        );
        let report = fs::read(path)
            .with_context(|| format!("failed to read DFM report {}", path.display()))?;
        let filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .with_context(|| format!("DFM report filename is not UTF-8: {}", path.display()))?;
        let compressed =
            zstd::encode_all(report.as_slice(), 19).context("failed to compress the DFM report")?;
        let viewer_url = format!(
            "{DFM_VIEWER_URL}#dfm=v1.{}&name={}",
            URL_SAFE_NO_PAD.encode(compressed),
            URL_SAFE_NO_PAD.encode(filename.as_bytes())
        );
        ensure!(
            viewer_url.len() <= MAX_DFM_URL_BYTES,
            "compressed DFM report exceeds the {} KiB automatic-open limit",
            MAX_DFM_URL_BYTES / 1024
        );
        let server = Server::http("127.0.0.1:0")
            .map_err(|error| anyhow::anyhow!(error.to_string()))
            .context("failed to start the local DFM report bridge")?;

        Ok(Self {
            server,
            root: format!("/{}", Uuid::new_v4().simple()),
            page: format!(
                r#"<!doctype html><meta charset=utf-8><script>location.replace("{viewer_url}")</script>"#
            )
            .into_bytes(),
        })
    }

    fn url(&self) -> String {
        let address = self
            .server
            .server_addr()
            .to_ip()
            .expect("DFM bridge uses an IP listener");
        format!("http://{address}{}", self.root)
    }

    fn serve(self, timeout: Duration) -> Result<()> {
        let request = self
            .server
            .recv_timeout(timeout)?
            .context("DFM report handoff timed out")?;
        ensure!(
            request.method() == &Method::Get && request.url() == self.root,
            "unexpected request to the local DFM report bridge"
        );
        request.respond(
            Response::from_data(self.page)
                .with_header(
                    Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
                )
                .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
                .with_header(Header::from_bytes("Referrer-Policy", "no-referrer").unwrap()),
        )?;
        Ok(())
    }
}

fn open_pcb_file(path: &Path) -> Result<()> {
    pcb_kicad::open_pcbnew(path).with_context(|| {
        format!(
            "Failed to open file in KiCad PCB Editor: {}",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_encodes_the_exact_report_and_filename() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("board ü.dfm.json");
        let expected = b"{\"verdict\":\"fail\"}\n\0exact";
        fs::write(&path, expected).unwrap();
        let bridge = DfmBridge::new(&path).unwrap();
        let page = std::str::from_utf8(&bridge.page).unwrap();
        let fragment = page
            .strip_prefix(
                r#"<!doctype html><meta charset=utf-8><script>location.replace("https://dfm.diode.computer/#dfm=v1."#,
            )
            .unwrap()
            .strip_suffix(r#"")</script>"#)
            .unwrap();
        let (report, filename) = fragment.split_once("&name=").unwrap();

        let report = URL_SAFE_NO_PAD.decode(report).unwrap();
        assert_eq!(zstd::decode_all(report.as_slice()).unwrap(), expected);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(filename).unwrap(),
            "board ü.dfm.json".as_bytes()
        );
    }
}
