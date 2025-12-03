use anyhow::Result;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct PackageArgs {
    /// Directory or file to package
    path: PathBuf,

    /// Output tar file path (optional)
    #[arg(short = 'o', long = "output")]
    output: Option<PathBuf>,

    /// Enable verbose output (shows file list and individual hashes)
    #[arg(short = 'v', long = "verbose")]
    verbose: bool,
}

pub fn execute(args: PackageArgs) -> Result<()> {
    let path = args.path.canonicalize()?;

    if !path.exists() {
        anyhow::bail!("Path does not exist: {}", path.display());
    }

    let is_file = path.is_file();
    println!(
        "Packaging {}: {}",
        if is_file { "file" } else { "directory" },
        path.display()
    );

    // If verbose, list what files will be included
    if args.verbose {
        println!("\nFiles included:");
        let entries = pcb_zen::canonical::list_canonical_tar_entries(&path)?;
        for entry in &entries {
            println!("  {}", entry);
        }
        println!("\nTotal: {} entries\n", entries.len());
    }

    // Write tar file if requested
    if let Some(output_path) = &args.output {
        let mut tar_data = Vec::new();
        pcb_zen::canonical::create_canonical_tar(&path, &mut tar_data)?;
        std::fs::write(output_path, &tar_data)?;
        println!("Wrote tar to: {}", output_path.display());
        println!("Tar size: {} bytes", tar_data.len());
    }

    // Compute and print content hash
    let content_hash = pcb_zen::canonical::compute_content_hash_from_dir(&path)?;
    println!("Content hash: {}", content_hash);

    // Compute manifest hash if pcb.toml exists (only for directories)
    if !is_file {
        let manifest_path = path.join("pcb.toml");
        if manifest_path.exists() {
            let manifest_content = std::fs::read_to_string(&manifest_path)?;
            let manifest_hash = pcb_zen::canonical::compute_manifest_hash(&manifest_content);
            println!("Manifest hash: {}", manifest_hash);
        }
    }

    Ok(())
}
