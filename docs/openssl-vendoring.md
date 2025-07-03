# OpenSSL Vendoring in PCB

This project vendors (statically links) OpenSSL to ensure consistent builds across different environments and to avoid runtime dependencies on system OpenSSL libraries.

## How it works

We use the `openssl-sys` crate with the `vendored` feature, which:
1. Downloads OpenSSL source code during the build process
2. Compiles OpenSSL from source
3. Statically links it into the final binary

## Configuration

The vendoring is configured in:
- `Cargo.toml`: Workspace dependency with `vendored` feature
- `crates/pcb-zen/Cargo.toml`: Direct dependency on `openssl-sys`
- `crates/pcb-zen-core/Cargo.toml`: Optional dependency enabled with `native` feature

## Build Requirements

To build with vendored OpenSSL, you need:
- C compiler (gcc, clang, or MSVC)
- make (on Unix-like systems)
- perl (for OpenSSL's build scripts)

On Ubuntu/Debian:
```bash
sudo apt-get install build-essential pkg-config perl
```

On macOS:
```bash
# Xcode Command Line Tools should provide everything needed
xcode-select --install
```

On Windows:
- Visual Studio with C++ build tools
- Strawberry Perl or ActivePerl

## Alternative: Using rustls

If you prefer to avoid OpenSSL entirely, you can switch to rustls (a pure Rust TLS implementation):

1. Update `Cargo.toml`:
```toml
reqwest = { version = "0.12.19", features = ["blocking", "json", "rustls-tls"], default-features = false }
```

2. Remove the `openssl-sys` dependencies

This would eliminate the need for C compilation and external dependencies, but may have different TLS compatibility characteristics.

## Troubleshooting

If you encounter build errors:
1. Ensure all build requirements are installed
2. Clear cargo cache: `cargo clean`
3. Check for conflicting OpenSSL installations
4. On macOS, you might need to set: `export OPENSSL_DIR=$(brew --prefix openssl)`