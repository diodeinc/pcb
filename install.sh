#!/usr/bin/env bash

# Get the directory where this script is located
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
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

# Check if any supported editors are installed
HAS_EDITOR=false
if command -v code &> /dev/null || command -v cursor &> /dev/null || command -v windsurf &> /dev/null; then
    HAS_EDITOR=true
fi

# Ask about VS Code extension installation
if [ "$HAS_EDITOR" = true ]; then
    echo ""
    echo -e "${YELLOW}Would you like to install the Zener VS Code extension?${NC}"
    echo "This will enable syntax highlighting and LSP support for .zen and .star files."
    read -p "Install extension? (y/N): " -n 1 -r
    echo ""
    
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "Installing VS Code extension..."
        if "$SCRIPT_DIR/vscode/install.sh"; then
            echo -e "${GREEN}✓ VS Code extension installed successfully!${NC}"
        else
            echo -e "${RED}✗ VS Code extension installation failed.${NC}"
            echo "You can try installing it manually later by running:"
            echo "  $SCRIPT_DIR/vscode/install.sh"
        fi
    else
        echo "Skipping VS Code extension installation."
        echo "You can install it later by running:"
        echo "  $SCRIPT_DIR/vscode/install.sh"
    fi
else
    echo ""
    echo -e "${YELLOW}Note: No supported editors found (VS Code, Cursor, or Windsurf).${NC}"
    echo "The VS Code extension was not installed."
    echo "After installing one of these editors, you can install the extension by running:"
    echo "  $SCRIPT_DIR/vscode/install.sh"
fi 