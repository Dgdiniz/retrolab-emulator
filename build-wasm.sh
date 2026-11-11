#!/bin/bash
# Builds the Mega Drive (sg1) and SNES (snes1) cores to WebAssembly with
# wasm-pack, into cores/<core>/wasm/pkg/, and copies them to
# web/public/pkg/, which the web debugger serves.
#
# Options:
#   --debug        Build the dev profile (the default is release).
#   --no-web       Leave web/public/pkg/ alone.
#   --sg1-only     Build only the Mega Drive core.
#   --snes1-only   Build only the SNES core.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SG1_WASM_CRATE_DIR="$SCRIPT_DIR/cores/sg1/wasm"
SNES1_WASM_CRATE_DIR="$SCRIPT_DIR/cores/snes1/wasm"
WEB_PKG_DIR="$SCRIPT_DIR/web/public/pkg"

PROFILE_FLAG="--release"
PROFILE_LABEL="release"
COPY_TO_WEB=true
BUILD_SG1=true
BUILD_SNES1=true

for arg in "$@"; do
    case $arg in
        --debug) PROFILE_FLAG=""; PROFILE_LABEL="debug" ;;
        --no-web) COPY_TO_WEB=false ;;
        --sg1-only) BUILD_SNES1=false ;;
        --snes1-only) BUILD_SG1=false ;;
        *) echo "Unknown option: $arg" >&2; exit 1 ;;
    esac
done

# Builds one crate and prints the size of its *_bg.wasm, raw and gzipped.
build_core() {
    local label="$1" crate_dir="$2" stem="$3"
    echo "Building $label ($PROFILE_LABEL)..."
    wasm-pack build $PROFILE_FLAG --target web --out-dir pkg "$crate_dir"
    local wasm_file="$crate_dir/pkg/${stem}_bg.wasm"
    if [ -f "$wasm_file" ]; then
        local raw gz
        raw=$(du -h "$wasm_file" | cut -f1)
        gz=$(gzip -c "$wasm_file" | wc -c | awk '{printf "%.2f MiB", $1/1048576}')
        echo "  -> $raw raw, $gz gzipped ($wasm_file)"
    fi
}

if [ "$BUILD_SG1" = true ]; then
    build_core "sg1" "$SG1_WASM_CRATE_DIR" "sg1_wasm"
fi
if [ "$BUILD_SNES1" = true ]; then
    build_core "snes1" "$SNES1_WASM_CRATE_DIR" "snes1_wasm"
fi

if [ "$COPY_TO_WEB" = true ]; then
    mkdir -p "$WEB_PKG_DIR"
    if [ "$BUILD_SG1" = true ]; then
        cp "$SG1_WASM_CRATE_DIR/pkg/"sg1_wasm* "$WEB_PKG_DIR/"
        # The web debugger's build reads package.json from public/pkg/.
        if [ -f "$SG1_WASM_CRATE_DIR/pkg/package.json" ]; then
            cp "$SG1_WASM_CRATE_DIR/pkg/package.json" "$WEB_PKG_DIR/"
        fi
    fi
    if [ "$BUILD_SNES1" = true ]; then
        cp "$SNES1_WASM_CRATE_DIR/pkg/"snes1_wasm* "$WEB_PKG_DIR/"
    fi
    echo "Copied to web/public/pkg/."
fi

echo ""
echo "To open the web debugger:"
echo "  cd web && npm install && npm run build && npm run serve   # http://localhost:8000"
