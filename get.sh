#!/usr/bin/env sh
# Ollama Deck - one-line installer for the prebuilt Linux build.
#
#   curl -fsSL https://raw.githubusercontent.com/mjb73/ollama-deck/main/get.sh | sh
#
# Installs the binary into ~/.local/bin and adds a menu entry. No root, no Rust.

set -eu

REPO="mjb73/ollama-deck"
ASSET="ollama-deck-linux-x86_64.tar.gz"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

say() { printf '%s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

case "$(uname -s)" in
    Linux) ;;
    *) die "this build is for Linux only" ;;
esac
case "$(uname -m)" in
    x86_64|amd64) ;;
    *) die "no prebuilt binary for $(uname -m) yet - build from source instead:
    git clone https://github.com/${REPO}.git && cd ollama-deck && ./install.sh" ;;
esac

URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

say "Downloading Ollama Deck..."
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$TMP/$ASSET" || die "download failed - see https://github.com/${REPO}/releases"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$TMP/$ASSET" "$URL" || die "download failed - see https://github.com/${REPO}/releases"
else
    die "neither curl nor wget is installed"
fi

tar -xzf "$TMP/$ASSET" -C "$TMP"
[ -f "$TMP/ollama-deck" ] || die "the download did not contain the program"

mkdir -p "$BIN_DIR"
install -m 755 "$TMP/ollama-deck" "$BIN_DIR/ollama-deck"
say "Installed to ${BIN_DIR}/ollama-deck"

mkdir -p "$APP_DIR"
cat > "$APP_DIR/ollama-deck.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Ollama Deck
Comment=Load, unload and watch Ollama models
Exec=${BIN_DIR}/ollama-deck
Icon=${ICON_DIR}/ollama-deck.svg
Terminal=false
Categories=Utility;System;
Keywords=ollama;llm;vram;gpu;
EOF

if [ -f "$TMP/assets/ollama-deck.svg" ]; then
    mkdir -p "$ICON_DIR"
    install -m 644 "$TMP/assets/ollama-deck.svg" "$ICON_DIR/ollama-deck.svg"
fi
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true

command -v ollama >/dev/null 2>&1 || say "note: ollama itself was not found - install it from https://ollama.com"

say ""
say "Done. Start it from your application menu, or type: ollama-deck"
case ":${PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *) say "(${BIN_DIR} is not on your PATH - add it, or run ${BIN_DIR}/ollama-deck directly.)" ;;
esac
