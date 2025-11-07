#!/usr/bin/env bash
set -euo pipefail

echo "Building WASM bundle for release..."

# Add wasm32-unknown-unknown target
rustup target add wasm32-unknown-unknown

# Get wasm-bindgen version from the project to ensure CLI matches
WASM_BINDGEN_VERSION=$(cargo tree -p pcb-zen-wasm -i wasm-bindgen | grep '^wasm-bindgen' | head -1 | awk '{print $2}' | sed 's/v//')
echo "Detected wasm-bindgen version: $WASM_BINDGEN_VERSION"

if [ -z "$WASM_BINDGEN_VERSION" ]; then
    echo "❌ Failed to detect wasm-bindgen version"
    exit 1
fi

# Install matching wasm-bindgen-cli
cargo install wasm-bindgen-cli --version "$WASM_BINDGEN_VERSION"

# Build pcb-zen-wasm with production optimizations (matching Bazel wasm-prod config)
echo "Building pcb-zen-wasm with production optimizations..."
RUSTFLAGS="-C opt-level=3 -C lto=fat -C embed-bitcode=yes -C codegen-units=1 -C target-feature=+tail-call" \
  cargo build --release --target wasm32-unknown-unknown -p pcb-zen-wasm

# Create output directory in target
mkdir -p target/wasm-bundle

# Run wasm-bindgen to generate JS bindings
echo "Running wasm-bindgen..."
wasm-bindgen target/wasm32-unknown-unknown/release/pcb_zen_wasm.wasm \
  --out-dir target/wasm-bundle \
  --target web \
  --out-name pcb_zen_wasm_bindgen

# Create tarball in target directory
echo "Creating wasm-bundle.tar.gz..."
tar -czf target/wasm-bundle.tar.gz -C target/wasm-bundle .

# Show what we created
echo "✅ WASM bundle created:"
ls -lh target/wasm-bundle.tar.gz
echo ""
echo "Bundle contains:"
tar -tzf target/wasm-bundle.tar.gz

echo "Done!"

