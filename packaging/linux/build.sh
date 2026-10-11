#!/usr/bin/env bash
# Build Linux packages: a portable .tar.gz, a .deb (needs `cargo install cargo-deb`) and,
# when `appimagetool` is on PATH, an AppImage. Output goes to dist/.
set -euo pipefail
cd "$(dirname "$0")/../.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
ARCH=$(uname -m)
cargo build --release -p diffcraft
mkdir -p dist
STAGE="dist/diffcraft-${VERSION}-linux-${ARCH}"
rm -rf "$STAGE" && mkdir -p "$STAGE"
cp target/release/diffcraft target/release/diffcraft-cli README.md LICENSE-MIT LICENSE-APACHE packaging/linux/diffcraft.desktop packaging/linux/diffcraft.png "$STAGE/"
tar -C dist -czf "$STAGE.tar.gz" "$(basename "$STAGE")"
echo "Wrote $STAGE.tar.gz"
if command -v cargo-deb >/dev/null 2>&1; then
    cargo deb -p diffcraft --no-build --output dist/
fi
if command -v appimagetool >/dev/null 2>&1; then
    APPDIR=dist/DiffCraft.AppDir
    rm -rf "$APPDIR" && mkdir -p "$APPDIR/usr/bin"
    cp target/release/diffcraft target/release/diffcraft-cli "$APPDIR/usr/bin/"
    cp packaging/linux/diffcraft.desktop packaging/linux/diffcraft.png "$APPDIR/"
    ln -sf usr/bin/diffcraft "$APPDIR/AppRun"
    ARCH="$ARCH" appimagetool "$APPDIR" "dist/DiffCraft-${VERSION}-${ARCH}.AppImage"
fi
