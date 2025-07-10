// This build script downloads buildifier binaries from the bazelbuild/buildtools project.
// Buildifier is licensed under the Apache License, Version 2.0.
// See: https://github.com/bazelbuild/buildtools

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;

const BUILDIFIER_VERSION: &str = "7.3.1";

// SHA256 checksums for buildifier binaries
const CHECKSUMS: &[(&str, &str)] = &[
    (
        "buildifier-darwin-amd64",
        "375f823103d01620aaec20a0c29c6cbca99f4fd0725ae30b93655c6704f44d71",
    ),
    (
        "buildifier-darwin-arm64",
        "5a6afc6ac7a09f5455ba0b89bd99d5ae23b4174dc5dc9d6c0ed5ce8caac3f813",
    ),
    (
        "buildifier-linux-amd64",
        "5474cc5128a74e806783d54081f581662c4be8ae65022f557e9281ed5dc88009",
    ),
    (
        "buildifier-linux-arm64",
        "0bf86c4bfffaf4f08eed77bde5b2082e4ae5039a11e2e8b03984c173c34a561c",
    ),
    (
        "buildifier-windows-amd64.exe",
        "370cd576075ad29930a82f5de132f1a1de4084c784a82514bd4da80c85acf4a8",
    ),
];

fn main() -> Result<()> {
    // Determine the target platform
    let target = env::var("TARGET").unwrap();
    let (platform, arch, extension) = match target.as_str() {
        "x86_64-apple-darwin" => ("darwin", "amd64", ""),
        "aarch64-apple-darwin" => ("darwin", "arm64", ""),
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => ("linux", "amd64", ""),
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => ("linux", "arm64", ""),
        "x86_64-pc-windows-msvc" | "x86_64-pc-windows-gnu" => ("windows", "amd64", ".exe"),
        _ => {
            eprintln!("Warning: Unsupported target '{target}', buildifier will not be bundled");
            return Ok(());
        }
    };

    let binary_name = format!("buildifier-{platform}-{arch}{extension}");
    let url = format!(
        "https://github.com/bazelbuild/buildtools/releases/download/v{BUILDIFIER_VERSION}/{binary_name}"
    );

    // Find the expected checksum
    let expected_checksum = CHECKSUMS
        .iter()
        .find(|(name, _)| *name == binary_name)
        .map(|(_, checksum)| *checksum)
        .context(format!("No checksum found for {binary_name}"))?;

    let out_dir = env::var("OUT_DIR")?;
    let buildifier_path = Path::new(&out_dir).join("buildifier");

    // Check if we already have the correct binary
    if buildifier_path.exists() {
        let contents = fs::read(&buildifier_path)?;
        let mut hasher = Sha256::new();
        hasher.update(&contents);
        let result = hasher.finalize();
        let actual_checksum = hex::encode(result);

        if actual_checksum == expected_checksum {
            println!("cargo:rerun-if-changed=build.rs");
            return Ok(());
        }
    }

    // Download the binary
    println!("cargo:info=Downloading buildifier {BUILDIFIER_VERSION} for {platform}-{arch}");

    let response = reqwest::blocking::get(&url)
        .with_context(|| format!("Failed to download buildifier from {url}"))?;

    if !response.status().is_success() {
        anyhow::bail!("Failed to download buildifier: HTTP {}", response.status());
    }

    let bytes = response.bytes()?;

    // Verify checksum
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let result = hasher.finalize();
    let actual_checksum = hex::encode(result);

    if actual_checksum != expected_checksum {
        anyhow::bail!(
            "Checksum mismatch for buildifier binary:\nExpected: {expected_checksum}\nActual: {actual_checksum}"
        );
    }

    // Write the binary
    let mut file = fs::File::create(&buildifier_path)?;
    file.write_all(&bytes)?;

    // Make it executable on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&buildifier_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&buildifier_path, perms)?;
    }

    println!("cargo:rerun-if-changed=build.rs");
    Ok(())
}
