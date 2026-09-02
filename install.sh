#!/usr/bin/env bash
set -euo pipefail

# mimori standalone installer
# Usage: curl -fsSL https://raw.githubusercontent.com/fusuyfusuy/mimori/main/install.sh | bash

TARGET_DIR="${HOME}/.local/bin"
TARGET_FILE="${TARGET_DIR}/mimori"
RAW_URL="https://raw.githubusercontent.com/fusuyfusuy/mimori/main/mimori"

echo "==> Installing mimori (Zero-Daemon Agent Context & Memory Engine)..."

# Ensure ~/.local/bin exists
mkdir -p "${TARGET_DIR}"

# Download latest executable
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "${RAW_URL}" -o "${TARGET_FILE}"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "${TARGET_FILE}" "${RAW_URL}"
else
    echo "Error: Neither curl nor wget was found. Please install either tool to proceed." >&2
    exit 1
fi

chmod +x "${TARGET_FILE}"

echo "==> Successfully installed mimori to ${TARGET_FILE}"

# Check PATH
if [[ ":$PATH:" != *":$TARGET_DIR:"* ]]; then
    echo ""
    echo "Note: ${TARGET_DIR} is not currently in your PATH."
    echo "Add the following line to your shell configuration profile (~/.bashrc, ~/.zshrc, etc.):"
    echo '  export PATH="$HOME/.local/bin:$PATH"'
    echo ""
fi

# Verify executable
"${TARGET_FILE}" --version 2>/dev/null || "${TARGET_FILE}" -h >/dev/null 2>&1 || true

echo "==> mimori is ready! Run 'mimori --help' or 'mimori dump --file' to get started."
