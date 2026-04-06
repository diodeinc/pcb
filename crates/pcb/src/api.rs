pub use pcb_diode_api::{AuthArgs, ScanArgs, SearchArgs, execute_scan, execute_search};

pub fn execute_auth(args: AuthArgs) -> anyhow::Result<()> {
    let ctx = pcb_diode_api::WorkspaceContext::from_cwd()?;
    pcb_diode_api::execute_auth(args, &ctx)
}
