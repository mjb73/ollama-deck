#!/usr/bin/env bash
# Ollama Deck installer: builds the widget and installs it for the current user.
set -euo pipefail
cd "$(dirname "$0")"

BIN_NAME="ollama-deck"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

if ! command -v cargo >/dev/null 2>&1; then
    cat <<'MSG'
Rust is not installed yet. Install it first (one line, no root needed):

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

then open a new terminal and run this script again.
MSG
    exit 1
fi

if ! command -v ollama >/dev/null 2>&1; then
    echo "note: ollama was not found on PATH - install it from https://ollama.com"
fi

echo "Building Ollama Deck (the first build takes a few minutes)..."
cargo build --release

echo "Installing to ${BIN_DIR}"
mkdir -p "${BIN_DIR}"
install -m 755 "target/release/${BIN_NAME}" "${BIN_DIR}/${BIN_NAME}"

mkdir -p "${APP_DIR}"
cat > "${APP_DIR}/${BIN_NAME}.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Ollama Deck
Comment=Load, unload and watch Ollama models
Exec=${BIN_DIR}/${BIN_NAME}
Icon=${ICON_DIR}/${BIN_NAME}.svg
Terminal=false
Categories=Utility;System;
Keywords=ollama;llm;vram;gpu;
EOF

if [ -f "assets/${BIN_NAME}.svg" ]; then
    mkdir -p "${ICON_DIR}"
    install -m 644 "assets/${BIN_NAME}.svg" "${ICON_DIR}/${BIN_NAME}.svg"
fi

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "${APP_DIR}" >/dev/null 2>&1 || true

echo
echo "Done. Look for \"Ollama Deck\" in your application menu,"
echo "or run: ${BIN_NAME}"
