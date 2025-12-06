//! WASI binary for testing pcb-zen-wasm evaluate logic.
//!
//! Usage: pcb-zen-wasi [zip_file] [main_file] [inputs_json]
//!
//! If zip_file is `-` or omitted, reads from stdin.
//! If main_file is omitted, auto-detects from boards/ directory.
//!
//! Example:
//!   cat src.zip | wasmtime run pcb-zen-wasi.wasm
//!   cat src.zip | wasmtime run pcb-zen-wasi.wasm - boards/foo/foo.zen '{}'

use std::io::Read;

fn main() {
    // Initialize logging to stderr for WASI
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .target(env_logger::Target::Stderr)
        .init();
    let args: Vec<String> = std::env::args().collect();

    let zip_path = args.get(1).map(|s| s.as_str()).unwrap_or("-");
    let main_file = args.get(2).map(|s| s.as_str()).unwrap_or("");
    let inputs_json = args.get(3).map(|s| s.as_str()).unwrap_or("{}");

    let zip_bytes = if zip_path == "-" {
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
            eprintln!("Failed to read zip from stdin: {}", e);
            std::process::exit(1);
        }
        buf
    } else {
        match std::fs::read(zip_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("Failed to read zip file '{}': {}", zip_path, e);
                std::process::exit(1);
            }
        }
    };

    match pcb_zen_wasm::evaluate_impl(zip_bytes, main_file, inputs_json) {
        Ok(result) => {
            let json = serde_json::to_string_pretty(&result).unwrap();
            println!("{}", json);
            if !result.success {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Evaluation error: {}", e);
            std::process::exit(1);
        }
    }
}
