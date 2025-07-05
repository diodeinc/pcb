#!/bin/bash

# Exit on error
set -e

# Get the directory where the script is located
SCRIPT_DIR="$(dirname "$0")"

# Colors for output
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
NC='\033[0m' # No Color

echo "Installing Zener VS Code extension dependencies..."
cd "$SCRIPT_DIR"

echo "Installing main dependencies..."
if ! npm install > /dev/null 2>&1; then
    echo -e "${RED}Failed to install npm dependencies${NC}"
    exit 1
fi

echo "Installing client dependencies..."
cd client
if ! npm install > /dev/null 2>&1; then
    echo -e "${RED}Failed to install client dependencies${NC}"
    exit 1
fi
cd ..

echo "Installing preview dependencies..."
cd preview
if ! npm install > /dev/null 2>&1; then
    echo -e "${RED}Failed to install preview dependencies${NC}"
    exit 1
fi
cd ..

echo "Compiling Zener VS Code extension..."
if ! npm run compile > /dev/null 2>&1; then
    echo -e "${RED}Failed to compile VS Code extension${NC}"
    echo "You may need to check TypeScript errors in the source code"
    exit 1
fi

echo "Packaging Zener VS Code extension..."
if ! npx --yes vsce package > /dev/null 2>&1; then
    echo -e "${RED}Failed to package VS Code extension${NC}"
    exit 1
fi

# Find the generated vsix file
VSIX_FILE=$(ls zener-*.vsix 2>/dev/null | head -n1)
if [ -z "$VSIX_FILE" ]; then
    echo -e "${RED}Failed to find packaged extension file${NC}"
    exit 1
fi

echo "Found extension package: $VSIX_FILE"

# Try to install in available editors
INSTALLED_IN=""

# Try VS Code
if command -v code &> /dev/null; then
    echo "Installing extension in VS Code..."
    if code --install-extension "$VSIX_FILE" > /dev/null 2>&1; then
        INSTALLED_IN="$INSTALLED_IN VS Code"
    else
        echo -e "${YELLOW}Warning: Failed to install in VS Code${NC}"
    fi
fi

# Try Cursor
if command -v cursor &> /dev/null; then
    echo "Installing extension in Cursor..."
    if cursor --install-extension "$VSIX_FILE" > /dev/null 2>&1; then
        INSTALLED_IN="$INSTALLED_IN Cursor"
    else
        echo -e "${YELLOW}Warning: Failed to install in Cursor${NC}"
    fi
fi

# Try Windsurf
if command -v windsurf &> /dev/null; then
    echo "Installing extension in Windsurf..."
    if windsurf --install-extension "$VSIX_FILE" > /dev/null 2>&1; then
        INSTALLED_IN="$INSTALLED_IN Windsurf"
    else
        echo -e "${YELLOW}Warning: Failed to install in Windsurf${NC}"
    fi
fi

# Check if we installed in any editor
if [ -z "$INSTALLED_IN" ]; then
    echo -e "${RED}Error: No supported editors found (VS Code, Cursor, or Windsurf)${NC}"
    echo "Please install one of these editors and try again"
    exit 1
fi

echo -e "${GREEN}Installation complete!${NC}"
echo "Extension installed in:$INSTALLED_IN"
