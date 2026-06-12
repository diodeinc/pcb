//! `rectifier` — infer and patch KiCad footprint 3D model rotate/offset from
//! STEP geometry. Rust port of `research/pose3d/solver.py`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

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
    name = "rectifier",
    about = "Infer and patch KiCad footprint 3D model transforms"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(ValueEnum, Clone, Debug)]
enum Mode {
    Loose,
    Strict,
}

#[derive(ValueEnum, Clone, Debug)]
enum Kind {
    All,
    Smd,
    Tht,
    Mixed,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Infer pose and patch one or more `.kicad_mod` files in place.
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
    Solve {
        /// Footprint file to evaluate.
        path: PathBuf,
        /// Emit ranked candidates, not just the top one.
        #[arg(long)]
        ranked: bool,
    },
    /// Audit footprints by showing benchmark failures in review-friendly form.
    /// Loose mode is the default; `--strict` requires exact rotation and
    /// ±0.10 mm L∞ offset. By default, audit uses the same deterministic
    /// randomized initial transform as `bench`. Default output groups failures
    /// by tier with up to `--top N` examples each; `--jsonl` emits flagged
    /// records, candidate correction records, apply errors, and a trailing
    /// summary for `| jq`.
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
    ///
    /// Each footprint's stored `(rotate ...)` / `(offset ...)` is treated as
    /// ground truth. By default, the solver is given a deterministic randomized
    /// initial transform so current-file priors cannot use the answer key.
    /// A footprint passes when the predicted rotation matches (exact or
    /// Z-rotation equivalent) and the predicted offset is within the mode's
    /// L∞ tolerance of the stored offset.
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
    // Default to silent: foxtrot's `triangulate` and the `step` parser emit
    // very chatty WARN/ERROR traces while tessellating STEP geometry that is
    // not actionable from here. Users can opt in with `RUST_LOG=warn` or
    // `RUST_LOG=rectifier=debug`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("off")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    match cli.command {
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
            mode: match mode {
                Mode::Loose => bench::BenchMode::Loose,
                Mode::Strict => bench::BenchMode::Strict,
            },
            kind: match kind {
                Kind::All => bench::BenchKindFilter::All,
                Kind::Smd => bench::BenchKindFilter::Smd,
                Kind::Tht => bench::BenchKindFilter::Tht,
                Kind::Mixed => bench::BenchKindFilter::Mixed,
            },
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
            kind: match kind {
                Kind::All => audit::AuditKindFilter::All,
                Kind::Smd => audit::AuditKindFilter::Smd,
                Kind::Tht => audit::AuditKindFilter::Tht,
                Kind::Mixed => audit::AuditKindFilter::Mixed,
            },
            limit,
            jobs,
            jsonl,
            top,
            apply,
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
