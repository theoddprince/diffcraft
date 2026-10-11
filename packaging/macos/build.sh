#!/usr/bin/env bash
# Build a universal (Apple Silicon + Intel) DiffCraft.app and a .dmg in dist/.
# Unsigned unless MACOS_SIGN_IDENTITY is set (then codesign is run with it).
set -euo pipefail
cd "$(dirname "$0")/../.."
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
rustup target add aarch64-apple-darwin x86_64-apple-darwin
for t in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p diffcraft --target "$t"
done
APP=dist/DiffCraft.app
rm -rf "$APP" && mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
for bin in diffcraft diffcraft-cli; do
    lipo -create -output "$APP/Contents/MacOS/$bin" \
        "target/aarch64-apple-darwin/release/$bin" "target/x86_64-apple-darwin/release/$bin"
done
# Icon: build an .icns from the PNG.
ICONSET=$(mktemp -d)/DiffCraft.iconset
mkdir -p "$ICONSET"
for s in 16 32 64 128 256; do
    sips -z $s $s packaging/icons/diffcraft.png --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    sips -z $((s*2)) $((s*2)) packaging/icons/diffcraft.png --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/DiffCraft.icns"
sed "s/@VERSION@/${VERSION}/g" packaging/macos/Info.plist > "$APP/Contents/Info.plist"
if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --deep --options runtime --sign "$MACOS_SIGN_IDENTITY" "$APP"
fi
hdiutil create -volname DiffCraft -srcfolder "$APP" -ov -format UDZO "dist/DiffCraft-${VERSION}-macos-universal.dmg"
echo "Wrote dist/DiffCraft-${VERSION}-macos-universal.dmg"
