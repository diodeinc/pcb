#!/usr/bin/env bash

# Get the directory where this script is located
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Colors for output
GREEN='\033[0;32m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo "Installing..."
echo ""

# Check if cargo is installed
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}Error: Cargo is not installed.${NC}"
    echo "Please install Rust and Cargo from https://rustup.rs/"
    exit 1
fi

# Run cargo install from the script directory
echo "Building and installing pcb binary..."
if cargo install --path "$SCRIPT_DIR/crates/pcb"; then
    echo ""
    echo -e "${GREEN}✓ Zener successfully installed!${NC}"
    echo ""
    echo "You can now use the 'pcb' command from anywhere."
    echo "Try running: pcb --help"
else
    echo ""
    echo -e "${RED}✗ Installation failed.${NC}"
    echo "Please check the error messages above."
    exit 1
fi 