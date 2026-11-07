#!/usr/bin/env sh
# Ollama Deck - one-line installer for the prebuilt Linux build.
#
#   curl -fsSL https://raw.githubusercontent.com/mjb73/ollama-deck/main/get.sh | sh
#
# Installs the program into ~/.local/bin and adds a menu entry.
# No root, no Rust, no build step.

set -eu

REPO="mjb73/ollama-deck"
ASSET="ollama-deck-linux-x86_64.tar.gz"
BIN="ollama-deck"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

say() { printf '%s\n' "$*"; }
warn() { printf '%s\n' "$*" >&2; }
die() { printf '\nerror: %s\n' "$*" >&2; exit 1; }

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- this machine

case "$(uname -s)" in
    Linux) ;;
    *) die "this build is for Linux; $(uname -s) is not supported" ;;
esac

case "$(uname -m)" in
    x86_64|amd64) ;;
    aarch64|arm64) die "there is no ready-made build for ARM yet. Build it yourself with:
    sudo apt install build-essential pkg-config libxkbcommon-dev libgl1-mesa-dev libwayland-dev
    git clone https://github.com/${REPO}.git && cd ${BIN} && ./install.sh" ;;
    *) die "there is no ready-made build for $(uname -m). See https://github.com/${REPO}" ;;
esac

if ! have curl && ! have wget; then
    die "neither curl nor wget is installed - install one of them and try again"
fi
if ! have tar; then
    die "tar is not installed - install it and try again"
fi

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t ollama-deck)"
trap 'rm -rf "$TMP"' EXIT INT TERM

# ---------------------------------------------------------------- download

fetch() { # fetch <url> <output file>
    if have curl; then
        if curl -fL --retry 3 --retry-delay 2 --connect-timeout 20 --max-time 300 \
            -sS "$1" -o "$2"; then
            return 0
        fi
    fi
    if have wget; then
        wget -q --tries=3 --timeout=30 -O "$2" "$1" && return 0
    fi
    return 1
}

have_archive() { # a real gzip tarball, not an error page
    [ -s "$1" ] || return 1
    if have gzip; then
        gzip -t "$1" >/dev/null 2>&1 || return 1
    fi
    tar -tzf "$1" >/dev/null 2>&1
}

# Releases are listed newest first. Pick the newest one that really carries
# the file: `releases/latest` alone breaks while an upload is still running,
# and after a release whose upload failed.
candidate_urls() {
    if have curl; then
        curl -fsSL --connect-timeout 15 --max-time 60 \
            "https://api.github.com/repos/${REPO}/releases?per_page=30" 2>/dev/null \
            | grep -o 'https://github.com/[^"]*'"${ASSET}" || true
    fi
    # Last resort: no API needed, works when the API quota is used up.
    printf '%s\n' "https://github.com/${REPO}/releases/latest/download/${ASSET}"
}

say "Downloading Ollama Deck..."
downloaded=""
for url in $(candidate_urls); do
    say "  ${url#https://github.com/}"
    if fetch "$url" "$TMP/$ASSET" 2>/dev/null && have_archive "$TMP/$ASSET"; then
        downloaded="$url"
        break
    fi
    rm -f "$TMP/$ASSET"
done

if [ -z "$downloaded" ]; then
    warn ""
    warn "error: the download did not work. Nothing was changed on this computer."
    warn ""
    warn "The file to fetch is:"
    warn "    https://github.com/${REPO}/releases/latest/download/${ASSET}"
    warn ""
    warn "Open https://github.com/${REPO}/releases in a browser. If the newest"
    warn "release has no ${ASSET} under Assets, wait a minute and run the"
    warn "line again - a build may still be uploading."
    if have cargo; then
        warn ""
        warn "Rust is installed on this computer, so you can build it instead:"
        warn "    git clone https://github.com/${REPO}.git && cd ${BIN} && ./install.sh"
    fi
    exit 1
fi

# ---------------------------------------------------------------- verify

sum_ok() { # sum_ok <release url> <file>; skipped when no checksum was published
    base="${1%/*}"
    if fetch "${base}/${ASSET}.sha256" "$TMP/$ASSET.sha256" 2>/dev/null && [ -s "$TMP/$ASSET.sha256" ]; then
        want="$(awk 'NR==1{print $1}' "$TMP/$ASSET.sha256")"
        if [ -n "$want" ]; then
            if have sha256sum; then
                got="$(sha256sum "$2" | awk '{print $1}')"
            elif have shasum; then
                got="$(shasum -a 256 "$2" | awk '{print $1}')"
            else
                return 0
            fi
            [ "$want" = "$got" ] || die "the download was damaged (checksum mismatch). Please run the line again."
            say "Checksum matches."
        fi
    fi
    return 0
}

sum_ok "$downloaded" "$TMP/$ASSET"

tar -xzf "$TMP/$ASSET" -C "$TMP" || die "the download could not be unpacked"
[ -f "$TMP/$BIN" ] || die "the download did not contain ${BIN}"
chmod +x "$TMP/$BIN"

# Does it start at all? `--help` needs no window, so a binary built for a
# newer C library is caught here, before anything is installed.
if ! self_test="$(LD_LIBRARY_PATH= "$TMP/$BIN" --help 2>&1)"; then
    case "$self_test" in
        *GLIBC*|*"libc.so"*)
            die "this build needs a newer system library than this distribution has.
Either update the distribution, or build it here from source:
    git clone https://github.com/${REPO}.git && cd ${BIN} && ./install.sh" ;;
        *)
            die "the downloaded program would not start:
${self_test}" ;;
    esac
fi
self_test="$(printf '%s\n' "$self_test" | head -n 1)"
say "Downloaded build: ${self_test}"

# egui draws through OpenGL, so a very small install can lack a library. Say
# which one instead of leaving the person with a window that never opens.
if have ldd; then
    missing="$(ldd "$TMP/$BIN" 2>/dev/null | grep 'not found' | awk '{print $1}' | sort -u | tr '\n' ' ')"
    if [ -n "$missing" ]; then
        warn ""
        warn "warning: these system libraries are missing: ${missing}"
        warn "The window cannot open without them. Install them with:"
        if have apt-get; then
            warn "  sudo apt install libxkbcommon0 libgl1 libx11-6 libxcursor1 libxi6 libxrandr2 libxinerama1 libwayland-client0"
        elif have dnf; then
            warn "  sudo dnf install libxkbcommon mesa-libGL libX11 libXcursor libXi libXrandr libXinerama wayland-libs-client"
        elif have pacman; then
            warn "  sudo pacman -S --needed libxkbcommon mesa libx11 libxcursor libxi libxrandr libxinerama wayland"
        elif have zypper; then
            warn "  sudo zypper install libxkbcommon0 libGL1 libX11-6 libXcursor1 libXi6 libXrandr2 libXinerama1 libwayland-client0"
        else
            warn "  use this distribution's package manager for the names above"
        fi
        warn ""
    fi
fi

# ---------------------------------------------------------------- install

mkdir -p "$BIN_DIR"
if have install; then
    install -m 755 "$TMP/$BIN" "$BIN_DIR/$BIN"
else
    cp "$TMP/$BIN" "$BIN_DIR/$BIN" && chmod 755 "$BIN_DIR/$BIN"
fi
say "Installed to ${BIN_DIR}/${BIN}"

mkdir -p "$APP_DIR"
cat > "$APP_DIR/${BIN}.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Ollama Deck
Comment=Load, unload and watch Ollama models
Exec=${BIN_DIR}/${BIN}
Icon=${ICON_DIR}/${BIN}.svg
Terminal=false
StartupWMClass=${BIN}
Categories=Utility;System;
Keywords=ollama;llm;vram;gpu;
EOF

if [ -f "$TMP/assets/${BIN}.svg" ]; then
    mkdir -p "$ICON_DIR"
    install -m 644 "$TMP/assets/${BIN}.svg" "$ICON_DIR/${BIN}.svg" 2>/dev/null \
        || cp "$TMP/assets/${BIN}.svg" "$ICON_DIR/${BIN}.svg"
fi
have update-desktop-database && update-desktop-database "$APP_DIR" >/dev/null 2>&1 || true

have ollama || say "note: ollama itself was not found - install it from https://ollama.com"

say ""
say "Done. Start it from your application menu, or type: ${BIN}"
case ":${PATH}:" in
    *":${BIN_DIR}:"*) ;;
    *) say "(${BIN_DIR} is not on your PATH - add it, or run ${BIN_DIR}/${BIN} directly.)" ;;
esac
