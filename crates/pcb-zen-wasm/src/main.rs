//! WASI binary for testing pcb-zen-wasm evaluate logic.
//!
//! Usage: pcb-zen-wasi [bundle_file] [main_file] [inputs_json]
//!
//! If bundle_file is `-` or omitted, reads from stdin.
//! If main_file is omitted, auto-detects from boards/ directory.
//!
//! Accepts source zips, release zips, and canonical `.tar.zst` bundles.
//! Release-style bundles are automatically normalized to their `src/` contents.
//!
//! Example:
//!   cat src.zip | wasmtime run pcb-zen-wasi.wasm
//!   cat release.zip | wasmtime run pcb-zen-wasi.wasm
//!   cat package.tar.zst | wasmtime run pcb-zen-wasi.wasm - boards/foo/foo.zen '{}'

use std::io::Read;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    let bundle_path = args.get(1).map(|s| s.as_str()).unwrap_or("-");
    let main_file = args.get(2).map(|s| s.as_str()).unwrap_or("");
    let inputs_json = args.get(3).map(|s| s.as_str()).unwrap_or("{}");

    let bundle_bytes = if bundle_path == "-" {
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).unwrap_or_else(|e| {
            eprintln!("Failed to read bundle from stdin: {e}");
            std::process::exit(1);
        });
        buf
    } else {
        std::fs::read(bundle_path).unwrap_or_else(|e| {
            eprintln!("Failed to read bundle file '{bundle_path}': {e}");
            std::process::exit(1);
        })
    };

    let result =
        pcb_zen_wasm::evaluate_impl(bundle_bytes, main_file, inputs_json).unwrap_or_else(|e| {
            eprintln!("Evaluation error: {e}");
            std::process::exit(1);
        });

    println!("{}", serde_json::to_string_pretty(&result).unwrap());
    if !result.success {
        std::process::exit(1);
    }
}
