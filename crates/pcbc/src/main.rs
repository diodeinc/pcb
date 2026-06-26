#[cfg(all(feature = "mimalloc", not(target_family = "wasm")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use clap::{Parser, Subcommand};
use colored::Colorize;
use env_logger::Env;
use std::ffi::OsString;
use std::process::Command;

mod bom;
mod build;
mod bundle;
mod changelog;
mod codegen;
mod config_input;
mod doc;
mod drc;
mod embed_step;
mod file_walker;
mod fmt;
mod gerber;
mod import;
mod info;
mod ipc2581;
mod kq;
mod layout;
mod list;
mod lsp;
mod migrate;
mod mod_cmd;
mod new;
mod open;
#[path = "mod/mod.rs"]
mod pcb_mod;
mod preview;
mod publish;
mod release;
mod remote_sandbox;
mod route;
mod sandbox_uri;
mod sim;
mod test;
mod update;
mod vendor;

mod profiling;
mod resolve;
mod tty;

#[derive(Parser)]
#[command(about = "PCB tool with build and layout capabilities", long_about = None)]
#[command(version)]
struct Cli {
    /// Enable debug logging
    #[arg(short = 'd', long = "debug", global = true, hide = true)]
    debug: bool,

    /// Write a performance profile to the specified path (Chrome tracing JSON format).
    /// View with chrome://tracing or https://ui.perfetto.dev/
    #[arg(long = "profile", global = true, value_name = "PATH", hide = true)]
    profile: Option<std::path::PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage authentication
    Auth(pcb_diode_api::AuthArgs),

    /// Build PCB projects
    #[command(alias = "b")]
    Build(build::BuildArgs),

    /// Run tests in .zen files
    #[command(alias = "t")]
    Test(test::TestArgs),

    /// Migrate PCB projects
    #[command(alias = "m")]
    Migrate(migrate::MigrateArgs),

    /// Manage package dependency manifests
    Mod(mod_cmd::ModArgs),

    /// Add or update a direct dependency
    Add(pcb_mod::ModAddArgs),

    /// Reconcile source imports and hydrate package dependency manifests
    Sync(pcb_mod::SyncArgs),

    /// List package dependency information
    List(list::ListArgs),

    /// Create a new board, package, or component
    New(new::NewArgs),

    /// Update dependencies to latest compatible versions
    Update(update::UpdateArgs),

    /// Generate Bill of Materials (BOM)
    Bom(bom::BomArgs),

    /// Display workspace and board information
    Info(info::InfoArgs),

    /// Import KiCad projects into a Zener board repository
    Import(import::ImportArgs),

    /// Generate package documentation
    Doc(doc::DocArgs),

    /// Print the pcb changelog
    #[command(hide = true)]
    Changelog(changelog::ChangelogArgs),

    /// Layout PCB designs
    #[command(alias = "l")]
    Layout(layout::LayoutArgs),

    /// Format .zen files
    Fmt(fmt::FmtArgs),

    /// Language Server Protocol support
    #[command(hide = true)]
    Lsp(lsp::LspArgs),

    /// Open PCB layout files
    #[command(alias = "o")]
    Open(open::OpenArgs),

    /// Publish packages and boards by creating version tags
    #[command(alias = "p")]
    Publish(publish::PublishArgs),

    /// Build and upload a preview release for a board
    Preview(preview::PreviewArgs),

    /// Vendor external dependencies
    Vendor(vendor::VendorArgs),

    /// Reserved subcommand for future use
    Fork,

    /// Embed a STEP model into a KiCad footprint
    EmbedStep(embed_step::EmbedStepArgs),

    /// Scan datasheets from local PDFs or URLs
    Scan(pcb_diode_api::ScanArgs),

    /// Search for electronic components
    Search(pcb_diode_api::SearchArgs),

    /// Auto-route PCB using DeepPCB cloud service
    #[command(hide = true)]
    Route(route::RouteArgs),

    /// Run SPICE simulations
    #[command(alias = "sim", alias = "s")]
    Simulate(sim::SimArgs),

    /// IPC-2581 parser and inspection tool
    #[command(alias = "ipc")]
    Ipc2581(ipc2581::Ipc2581Args),

    /// Gerber X2 parser and rendering tool
    Gerber(gerber::GerberArgs),

    /// Inspect KiCad symbol libraries as structured JSON
    #[command(hide = true)]
    Kq(kq::KqArgs),

    /// External subcommands are forwarded to pcb-<command>
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {e}", "Error:".red());
        for cause in e.chain().skip(1) {
            eprintln!("  {cause}");
        }
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cli = if let Some(arg0) = std::env::var_os("PCB_SHIM_ARG0") {
        Cli::parse_from(std::iter::once(arg0).chain(std::env::args_os().skip(1)))
    } else {
        Cli::parse()
    };

    // Initialize logger with default level depending on --debug (overridden by RUST_LOG)
    // Must happen before tracing subscriber to avoid conflicts
    let env = if cli.debug {
        Env::default().default_filter_or("debug")
    } else {
        Env::default().default_filter_or("error")
    };
    env_logger::Builder::from_env(env).init();

    // Initialize profiling if --profile is passed (guard must be held until end of run)
    let _profile_guard = profiling::init(cli.profile);

    match cli.command {
        Commands::Auth(args) => {
            let ctx = pcb_diode_api::WorkspaceContext::from_cwd()?;
            pcb_diode_api::execute_auth(args, &ctx)
        }
        Commands::Build(args) => build::execute(args),
        Commands::Test(args) => test::execute(args),
        Commands::Migrate(args) => migrate::execute(args),
        Commands::Mod(args) => mod_cmd::execute(args),
        Commands::Add(args) => pcb_mod::execute_mod_add(args),
        Commands::Sync(args) => pcb_mod::execute_sync(args),
        Commands::List(args) => list::execute(args),
        Commands::New(args) => new::execute(args),
        Commands::Update(args) => update::execute(args),
        Commands::Bom(args) => bom::execute(args),
        Commands::Info(args) => info::execute(args),
        Commands::Import(args) => import::execute(args),
        Commands::Doc(args) => doc::execute(args),
        Commands::Changelog(args) => changelog::execute(args),
        Commands::Layout(args) => layout::execute(args),
        Commands::Fmt(args) => fmt::execute(args),
        Commands::Lsp(args) => lsp::execute(args),
        Commands::Open(args) => open::execute(args),
        Commands::Publish(args) => publish::execute(args),
        Commands::Preview(args) => preview::execute(args),
        Commands::Vendor(args) => vendor::execute(args),
        Commands::Fork => {
            println!("`pcb fork` is a reserved subcommand for future use.");
            Ok(())
        }
        Commands::Scan(args) => pcb_diode_api::execute_scan(args),
        Commands::Search(args) => pcb_diode_api::execute_search(args),
        Commands::EmbedStep(args) => embed_step::execute(args),
        Commands::Route(args) => route::execute(args),
        Commands::Simulate(args) => sim::execute(args),
        Commands::Ipc2581(args) => ipc2581::execute(args),
        Commands::Gerber(args) => gerber::execute(args),
        Commands::Kq(args) => kq::execute(args),
        Commands::External(args) => execute_external(args),
    }
}

fn execute_external(args: Vec<OsString>) -> anyhow::Result<()> {
    if args.is_empty() {
        anyhow::bail!("No external command specified");
    }

    // First argument is the subcommand name.
    let command = args[0].to_string_lossy();
    let external_args = &args[1..];
    let candidates = external_command_candidates(&command);

    // First search PATH, which supports separately-installed extensions.
    for external_cmd in &candidates {
        match run_external_command(external_cmd, external_args) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                anyhow::bail!(
                    "Failed to execute external command '{}': {}",
                    external_cmd,
                    e
                )
            }
            Err(_) => {}
        }
    }

    // Then search next to the currently-running `pcbc` binary. Bundled extension
    // binaries installed by the `pcb` shim live in that toolchain directory,
    // which is not necessarily on PATH.
    for external_cmd in &candidates {
        if let Some(sibling) = sibling_external_command(external_cmd) {
            match run_external_command(sibling, external_args) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    anyhow::bail!(
                        "Failed to execute external command '{}': {}",
                        external_cmd,
                        e
                    )
                }
                Err(_) => {}
            }
        }
    }

    eprintln!("Error: Unknown command '{command}'");
    eprintln!(
        "No built-in command or external command '{}' found",
        candidates.join("' / '")
    );
    std::process::exit(1);
}

fn external_command_candidates(command: &str) -> Vec<String> {
    // Both first-party bundled sidecars (e.g. `pcb-rectify`) and third-party
    // extensions follow the `pcb-<command>` naming convention. Bundled sidecars
    // are installed next to `pcbc` in the toolchain dir and found by the sibling
    // search; extensions are found on PATH.
    vec![format!("pcb-{command}")]
}

fn run_external_command<S: AsRef<std::ffi::OsStr>>(
    program: S,
    args: &[OsString],
) -> std::io::Result<()> {
    let status = Command::new(program).args(args).status()?;
    if !status.success() {
        match status.code() {
            Some(code) => std::process::exit(code),
            None => {
                return Err(std::io::Error::other(
                    "External command terminated by signal",
                ));
            }
        }
    }
    Ok(())
}

fn sibling_external_command(command: &str) -> Option<std::path::PathBuf> {
    let current = std::env::current_exe().ok()?;
    let parent = current.parent()?;
    let binary_name = if cfg!(windows) {
        format!("{command}.exe")
    } else {
        command.to_string()
    };
    let sibling = parent.join(binary_name);
    sibling.is_file().then_some(sibling)
}
