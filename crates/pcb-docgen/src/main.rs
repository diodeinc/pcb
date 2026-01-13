//! CLI for generating Zener package documentation.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: pcb-docgen <package_path>");
        eprintln!();
        eprintln!("Generate markdown documentation for a Zener package.");
        eprintln!("Output is written to stdout.");
        std::process::exit(1);
    }

    let package_root = PathBuf::from(&args[1]);

    if !package_root.exists() {
        eprintln!("Error: Path does not exist: {}", package_root.display());
        std::process::exit(1);
    }

    let result = pcb_docgen::generate_docs(&package_root, None, None, None)?;

    // Output markdown to stdout
    print!("{}", result.markdown);

    // Stats to stderr
    eprintln!(
        "Generated docs for {} libraries and {} modules",
        result.library_count, result.module_count
    );

    Ok(())
}
