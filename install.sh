#!/usr/bin/env bash

# Get the directory where this script is located
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

JOBS=""

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        -j)
            if [[ -z "${2:-}" || "$2" == -* ]]; then
                echo -e "${RED}Error: -j requires a number (e.g. -j 4).${NC}"
                echo "Usage: $0 [-j N]"
                exit 1
            fi
            if ! [[ "$2" =~ ^[0-9]+$ ]] || [[ "$2" -le 0 ]]; then
                echo -e "${RED}Error: -j must be a positive integer (got: $2).${NC}"
                echo "Usage: $0 [-j N]"
                exit 1
            fi
            JOBS="$2"
            shift 2
            ;;
        *)
            echo -e "${RED}Unknown argument: $1${NC}"
            echo "Usage: $0 [-j N]"
            exit 1
            ;;
    esac
done

echo "Installing..."
echo ""

# Check if cargo is installed
if ! command -v cargo &> /dev/null; then
    echo -e "${RED}Error: Cargo is not installed.${NC}"
    echo "Please install Rust and Cargo from https://rustup.rs/"
    exit 1
fi

# Build command
CARGO_CMD=(cargo install --path "$SCRIPT_DIR/crates/pcb")

if [[ -n "$JOBS" ]]; then
    echo -e "${YELLOW}Limiting build to $JOBS threads${NC}"
    CARGO_CMD+=( -j "$JOBS" )
fi

echo "Building and installing pcb binary..."

if "${CARGO_CMD[@]}"; then
    echo ""
    echo -e "${GREEN}✓ Zener successfully installed!${NC}"
    echo ""
    echo "You can now use the 'pcb' command from anywhere."
    echo "Try running: pcb --help"
else
    echo ""
    echo -e "${RED}✗ Installation failed.${NC}"
    echo -e "${YELLOW} If the build failed because of memory issues, you can try limiting the number of jobs using the -j flag."
    echo -e "Try running: ./install.sh -j 1 ${NC}"
    echo ""
    echo "Please check the error messages above."
    exit 1
fi
