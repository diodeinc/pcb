use anyhow::Result;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use blake3::Hasher;
use clap::Args;
use std::path::PathBuf;

#[derive(Args)]
pub struct PackageArgs {
    /// Directory to package
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

    if !path.is_dir() {
        anyhow::bail!("Path must be a directory: {}", path.display());
    }

    println!("Packaging: {}", path.display());

    // If verbose, list what files will be included
    if args.verbose {
        println!("\nFiles included:");
        let entries = pcb_zen::canonical::list_canonical_tar_entries(&path)?;
        for entry in &entries {
            println!("  {}", entry);
        }
        println!("\nTotal: {} entries\n", entries.len());
    }

    // Create canonical tar and compute hash
    let mut hasher = Hasher::new();
    let mut tar_data = Vec::new();

    if args.output.is_some() {
        // Need to buffer the tar data to write to file
        let cursor = std::io::Cursor::new(&mut tar_data);
        let mut multi_writer = MultiWriter::new(&mut hasher, cursor);
        pcb_zen::canonical::create_canonical_tar(&path, &mut multi_writer)?;
    } else {
        // Just stream to hasher
        pcb_zen::canonical::create_canonical_tar(&path, &mut hasher)?;
    }

    let hash = hasher.finalize();
    let hash_b64 = BASE64.encode(hash.as_bytes());

    println!("Content hash: h1:{}", hash_b64);

    // Compute manifest hash if pcb.toml exists
    let manifest_path = path.join("pcb.toml");
    if manifest_path.exists() {
        let manifest_content = std::fs::read(&manifest_path)?;
        let manifest_hash = blake3::hash(&manifest_content);
        let manifest_hash_b64 = BASE64.encode(manifest_hash.as_bytes());
        println!("Manifest hash: h1:{}", manifest_hash_b64);
    }

    // Write tar file if requested
    if let Some(output_path) = args.output {
        std::fs::write(&output_path, &tar_data)?;
        println!("Wrote tar to: {}", output_path.display());
        println!("Tar size: {} bytes", tar_data.len());
    }

    Ok(())
}

/// MultiWriter that writes to both a hasher and another writer
struct MultiWriter<'a, W: std::io::Write> {
    hasher: &'a mut Hasher,
    writer: W,
}

impl<'a, W: std::io::Write> MultiWriter<'a, W> {
    fn new(hasher: &'a mut Hasher, writer: W) -> Self {
        Self { hasher, writer }
    }
}

impl<'a, W: std::io::Write> std::io::Write for MultiWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.hasher.update(buf);
        self.writer.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
