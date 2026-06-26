//! `pcb rectify` — infer and patch KiCad footprint 3D model rotate/offset from
//! STEP geometry. Rust port of `research/pose3d/solver.py`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};

mod audit;
mod bench;
mod footprint;
mod fs_util;
mod mesh;
mod patch;
mod pose;
mod progress;
mod raster;
mod solver;

#[derive(Parser, Debug)]
#[command(
    name = "pcb-rectify",
    bin_name = "pcb rectify",
    about = "Check footprint <-> 3d model alignment in .kicad_mod file"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Mode {
    Loose,
    Strict,
}

#[derive(ValueEnum, Clone, Copy, Debug)]
enum Kind {
    All,
    Smd,
    Tht,
    Mixed,
}

#[derive(ClapArgs, Debug)]
struct CheckArgs {
    /// Footprint files and/or directories. Directories are searched
    /// recursively for `.kicad_mod` files.
    paths: Vec<PathBuf>,
    /// Restrict the check to one footprint kind.
    #[arg(long, value_enum, default_value_t = Kind::All)]
    kind: Kind,
    /// Stop after N footprints (default: all).
    #[arg(long)]
    limit: Option<usize>,
    /// Override rayon's global thread count.
    #[arg(long)]
    jobs: Option<usize>,
    /// Emit one JSON record per flagged footprint.
    #[arg(long)]
    jsonl: bool,
    /// Limit examples per failure tier (0 = show all).
    #[arg(long, default_value_t = 0)]
    top: usize,
    /// Use strict criteria: exact rotation and ±0.10 mm L∞ offset.
    #[arg(long)]
    strict: bool,
    /// Use each footprint's stored transform as the solver's initial
    /// transform. This restores the legacy audit behavior.
    #[arg(long)]
    use_stored_initial_transform: bool,
    /// Seed for deterministic benchmark initial-transform randomization.
    #[arg(long, default_value_t = bench::DEFAULT_INITIAL_TRANSFORM_SEED)]
    initial_transform_seed: u64,
}

#[derive(ClapArgs, Debug)]
struct FixArgs {
    /// Footprint files and/or directories. Directories are searched
    /// recursively for `.kicad_mod` files.
    paths: Vec<PathBuf>,
    /// Restrict the fix to one footprint kind.
    #[arg(long, value_enum, default_value_t = Kind::All)]
    kind: Kind,
    /// Stop after N footprints (default: all).
    #[arg(long)]
    limit: Option<usize>,
    /// Override rayon's global thread count.
    #[arg(long)]
    jobs: Option<usize>,
    /// Emit one JSON record per flagged footprint and applied correction.
    #[arg(long)]
    jsonl: bool,
    /// Limit examples per failure tier (0 = show all).
    #[arg(long, default_value_t = 0)]
    top: usize,
    /// Use strict criteria: exact rotation and ±0.10 mm L∞ offset.
    #[arg(long)]
    strict: bool,
    /// Use each footprint's stored transform as the solver's initial
    /// transform. This restores the legacy audit behavior.
    #[arg(long)]
    use_stored_initial_transform: bool,
    /// Seed for deterministic benchmark initial-transform randomization.
    #[arg(long, default_value_t = bench::DEFAULT_INITIAL_TRANSFORM_SEED)]
    initial_transform_seed: u64,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Check footprints and exit non-zero when model transforms look wrong.
    Check(CheckArgs),
    /// Patch flagged footprint model transforms in place.
    Fix(FixArgs),

    /// Infer pose and patch one or more `.kicad_mod` files in place.
    #[command(hide = true)]
    Patch {
        /// Footprint files or directories to patch.
        paths: Vec<PathBuf>,
        /// Report predicted transforms without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Write a backup copy alongside each patched file.
        #[arg(long)]
        backup: bool,
        #[arg(long, default_value = ".bak")]
        backup_suffix: String,
        /// Print previous transform details.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Infer pose and emit the top candidate as JSON (parity oracle).
    #[command(hide = true)]
    Solve {
        /// Footprint file to evaluate.
        path: PathBuf,
        /// Emit ranked candidates, not just the top one.
        #[arg(long)]
        ranked: bool,
    },
    /// Audit footprints by showing benchmark failures in review-friendly form.
    #[command(hide = true)]
    Audit {
        /// Footprint files and/or directories. Directories are searched
        /// recursively for `.kicad_mod` files.
        paths: Vec<PathBuf>,
        /// Restrict the audit to one footprint kind.
        #[arg(long, value_enum, default_value_t = Kind::All)]
        kind: Kind,
        /// Stop after N footprints (default: all).
        #[arg(long)]
        limit: Option<usize>,
        /// Override rayon's global thread count.
        #[arg(long)]
        jobs: Option<usize>,
        /// Emit one JSON record per flagged footprint.
        #[arg(long)]
        jsonl: bool,
        /// Limit examples per failure tier (0 = show all).
        #[arg(long, default_value_t = 0)]
        top: usize,
        /// Apply candidate corrections for flagged failures in place.
        #[arg(long)]
        apply: bool,
        /// Use strict benchmark criteria: exact rotation and ±0.10 mm L∞ offset.
        #[arg(long)]
        strict: bool,
        /// Use each footprint's stored transform as the solver's initial
        /// transform. This restores the legacy audit behavior.
        #[arg(long)]
        use_stored_initial_transform: bool,
        /// Seed for deterministic benchmark initial-transform randomization.
        #[arg(long, default_value_t = bench::DEFAULT_INITIAL_TRANSFORM_SEED)]
        initial_transform_seed: u64,
    },
    /// Evaluate the solver against a set of `.kicad_mod` files on disk.
    #[command(hide = true)]
    Bench {
        /// Footprint files and/or directories. Directories are searched
        /// recursively for `.kicad_mod` files.
        paths: Vec<PathBuf>,
        /// Benchmark strictness: `loose` (±0.20 mm, Z-rotation OK) or
        /// `strict` (±0.10 mm, exact rotation only).
        #[arg(long, value_enum, default_value_t = Mode::Loose)]
        mode: Mode,
        /// Restrict the benchmark to one footprint kind.
        #[arg(long, value_enum, default_value_t = Kind::All)]
        kind: Kind,
        /// Stop after N footprints (default: all).
        #[arg(long)]
        limit: Option<usize>,
        /// Override rayon's global thread count.
        #[arg(long)]
        jobs: Option<usize>,
        /// Emit one JSON record per footprint + a trailing summary record.
        #[arg(long)]
        jsonl: bool,
        /// Use each footprint's stored transform as the solver's initial
        /// transform. This restores the legacy benchmark behavior.
        #[arg(long)]
        use_stored_initial_transform: bool,
        /// Seed for deterministic benchmark initial-transform randomization.
        #[arg(long, default_value_t = bench::DEFAULT_INITIAL_TRANSFORM_SEED)]
        initial_transform_seed: u64,
    },
}

fn main() -> Result<()> {
    // Default to silent: foxtrot's STEP parser can emit noisy diagnostics that
    // are not actionable here. Users can opt in with RUST_LOG=warn or a
    // narrower filter.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("off"))
        .format_timestamp(None)
        .try_init()
        .ok();

    let cli = Cli::parse();
    match cli.command {
        Command::Check(args) => run_check(args),
        Command::Fix(args) => run_fix(args),
        Command::Patch {
            paths,
            dry_run,
            backup,
            backup_suffix,
            verbose,
        } => patch::run(patch::Args {
            paths,
            dry_run,
            backup,
            backup_suffix,
            verbose,
        }),
        Command::Solve { path, ranked } => {
            let report = solver::solve_json(&path, ranked)
                .with_context(|| format!("solve failed for {}", path.display()))?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Bench {
            paths,
            mode,
            kind,
            limit,
            jobs,
            jsonl,
            use_stored_initial_transform,
            initial_transform_seed,
        } => bench::run(bench::Args {
            paths,
            mode: bench_mode(mode),
            kind: bench_kind(kind),
            limit,
            jobs,
            jsonl,
            randomize_initial_transform: !use_stored_initial_transform,
            initial_transform_seed,
        }),
        Command::Audit {
            paths,
            kind,
            limit,
            jobs,
            jsonl,
            top,
            apply,
            strict,
            use_stored_initial_transform,
            initial_transform_seed,
        } => audit::run(audit::Args {
            paths,
            kind: audit_kind(kind),
            limit,
            jobs,
            jsonl,
            top,
            apply,
            fail_on_flagged: false,
            mode: if strict {
                bench::BenchMode::Strict
            } else {
                bench::BenchMode::Loose
            },
            randomize_initial_transform: !use_stored_initial_transform,
            initial_transform_seed,
        }),
    }
}

fn run_check(args: CheckArgs) -> Result<()> {
    audit::run(audit::Args {
        paths: args.paths,
        kind: audit_kind(args.kind),
        limit: args.limit,
        jobs: args.jobs,
        jsonl: args.jsonl,
        top: args.top,
        apply: false,
        fail_on_flagged: true,
        mode: if args.strict {
            bench::BenchMode::Strict
        } else {
            bench::BenchMode::Loose
        },
        randomize_initial_transform: !args.use_stored_initial_transform,
        initial_transform_seed: args.initial_transform_seed,
    })
}

fn run_fix(args: FixArgs) -> Result<()> {
    audit::run(audit::Args {
        paths: args.paths,
        kind: audit_kind(args.kind),
        limit: args.limit,
        jobs: args.jobs,
        jsonl: args.jsonl,
        top: args.top,
        apply: true,
        fail_on_flagged: false,
        mode: if args.strict {
            bench::BenchMode::Strict
        } else {
            bench::BenchMode::Loose
        },
        randomize_initial_transform: !args.use_stored_initial_transform,
        initial_transform_seed: args.initial_transform_seed,
    })
}

fn audit_kind(kind: Kind) -> audit::AuditKindFilter {
    match kind {
        Kind::All => audit::AuditKindFilter::All,
        Kind::Smd => audit::AuditKindFilter::Smd,
        Kind::Tht => audit::AuditKindFilter::Tht,
        Kind::Mixed => audit::AuditKindFilter::Mixed,
    }
}

fn bench_kind(kind: Kind) -> bench::BenchKindFilter {
    match kind {
        Kind::All => bench::BenchKindFilter::All,
        Kind::Smd => bench::BenchKindFilter::Smd,
        Kind::Tht => bench::BenchKindFilter::Tht,
        Kind::Mixed => bench::BenchKindFilter::Mixed,
    }
}

fn bench_mode(mode: Mode) -> bench::BenchMode {
    match mode {
        Mode::Loose => bench::BenchMode::Loose,
        Mode::Strict => bench::BenchMode::Strict,
    }
}
