#[cfg(all(feature = "mimalloc", not(target_family = "wasm")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use clap::{Parser, Subcommand};
use colored::Colorize;
use env_logger::Env;
use std::ffi::OsString;
use std::process::Command;

#[cfg(feature = "api")]
mod api;
mod bom;
mod build;
mod doc;
mod drc;
mod file_walker;
mod fmt;
mod fork;
mod info;
mod ipc2581;
mod layout;
mod lsp;
mod mcp;
mod migrate;
mod new;
mod open;
mod package;
mod publish;
mod release;
#[cfg(feature = "api")]
mod route;
mod self_update;
mod sim;
mod tag;
mod test;
mod update;
mod vendor;

mod profiling;
mod resolve;

#[derive(Parser)]
#[command(name = "pcb")]
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
    #[cfg(feature = "api")]
    Auth(api::AuthArgs),

    /// Build PCB projects
    #[command(alias = "b")]
    Build(build::BuildArgs),

    /// Run tests in .zen files
    #[command(alias = "t")]
    Test(test::TestArgs),

    /// Migrate PCB projects
    #[command(alias = "m")]
    Migrate(migrate::MigrateArgs),

    /// Create a new workspace, board, package, or component
    New(new::NewArgs),

    /// Update dependencies to latest compatible versions
    Update(update::UpdateArgs),

    /// Update the pcb tool itself
    #[command(name = "self")]
    SelfUpdate(self_update::SelfUpdateArgs),

    /// Generate Bill of Materials (BOM)
    Bom(bom::BomArgs),

    /// Display workspace and board information
    Info(info::InfoArgs),

    /// View embedded Zener documentation
    Doc(doc::DocArgs),

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

    /// Publish packages by creating version tags
    #[command(alias = "p")]
    Publish(publish::PublishArgs),

    /// Release PCB project versions
    #[command(alias = "r")]
    Release(release::ReleaseArgs),

    /// Create and manage PCB version tags
    Tag(tag::TagArgs),

    /// Vendor external dependencies
    Vendor(vendor::VendorArgs),

    /// Manage forked dependencies for local development
    Fork(fork::ForkArgs),

    /// Scan PDF datasheets with OCR
    #[cfg(feature = "api")]
    Scan(api::ScanArgs),

    /// Search for electronic components
    #[cfg(feature = "api")]
    Search(api::SearchArgs),

    /// Auto-route PCB using DeepPCB cloud service
    #[cfg(feature = "api")]
    #[command(hide = true)]
    Route(route::RouteArgs),

    /// Run SPICE simulations
    Sim(sim::SimArgs),

    /// Start the Model Context Protocol (MCP) server
    #[command(hide = true)]
    Mcp(mcp::McpArgs),

    /// IPC-2581 parser and inspection tool
    Ipc2581(ipc2581::Ipc2581Args),

    /// Create canonical tar package and compute hash (debug tool)
    #[command(hide = true)]
    Package(package::PackageArgs),

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
    let cli = Cli::parse();

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

    // Skip auto-update check in CI environments or when running the update command
    if std::env::var("CI").is_err() && !is_update_command(&cli.command) {
        check_and_update();
    }

    match cli.command {
        #[cfg(feature = "api")]
        Commands::Auth(args) => api::execute_auth(args),
        Commands::Build(args) => build::execute(args),
        Commands::Test(args) => test::execute(args),
        Commands::Migrate(args) => migrate::execute(args),
        Commands::New(args) => new::execute(args),
        Commands::Update(args) => update::execute(args),
        Commands::SelfUpdate(args) => self_update::execute(args),
        Commands::Bom(args) => bom::execute(args),
        Commands::Info(args) => info::execute(args),
        Commands::Doc(args) => doc::execute(args),
        Commands::Layout(args) => layout::execute(args),
        Commands::Fmt(args) => fmt::execute(args),
        Commands::Lsp(args) => lsp::execute(args),
        Commands::Open(args) => open::execute(args),
        Commands::Publish(args) => publish::execute(args),
        Commands::Release(args) => release::execute(args),
        Commands::Tag(args) => tag::execute(args),
        Commands::Vendor(args) => vendor::execute(args),
        Commands::Fork(args) => fork::execute(args),
        #[cfg(feature = "api")]
        Commands::Scan(args) => api::execute_scan(args),
        #[cfg(feature = "api")]
        Commands::Search(args) => api::execute_search(args),
        #[cfg(feature = "api")]
        Commands::Route(args) => route::execute(args),
        Commands::Sim(args) => sim::execute(args),
        Commands::Mcp(args) => mcp::execute(args),
        Commands::Ipc2581(args) => ipc2581::execute(args),
        Commands::Package(args) => package::execute(args),
        Commands::External(args) => {
            if args.is_empty() {
                anyhow::bail!("No external command specified");
            }

            // First argument is the subcommand name
            let command = args[0].to_string_lossy();
            let external_cmd = format!("pcb-{command}");

            // Try to find and execute the external command
            match Command::new(&external_cmd).args(&args[1..]).status() {
                Ok(status) => {
                    // Forward the exit status
                    if !status.success() {
                        match status.code() {
                            Some(code) => std::process::exit(code),
                            None => anyhow::bail!(
                                "External command '{}' terminated by signal",
                                external_cmd
                            ),
                        }
                    }
                    Ok(())
                }
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        eprintln!("Error: Unknown command '{command}'");
                        eprintln!("No built-in command or external command '{external_cmd}' found");
                        std::process::exit(1);
                    } else {
                        anyhow::bail!(
                            "Failed to execute external command '{}': {}",
                            external_cmd,
                            e
                        )
                    }
                }
            }
        }
    }
}

fn is_update_command(command: &Commands) -> bool {
    matches!(command, Commands::Update(_) | Commands::SelfUpdate(_))
}

fn check_and_update() {
    let mut updater = axoupdater::AxoUpdater::new_for("pcb");
    if let Ok(updater) = updater.load_receipt() {
        if let Ok(true) = updater.is_update_needed_sync() {
            eprintln!("{}", "A new version of pcb is available!".blue().bold());
            eprintln!("Run {} to update.", "pcb self update".yellow().bold());
        }
    }
}
