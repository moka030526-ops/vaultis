#!/usr/bin/env bash
# Assemble vaultis.app from already-built binaries. Runs on macOS only (it needs lipo,
# sips, iconutil and codesign, which ship with the Xcode command-line tools).
#
#   packaging/macos/make-app.sh OUT_DIR VERSION GUI_BIN CLI_BIN SAMPLE_VAULT_DIR
#
#   OUT_DIR           where vaultis.app is written (an existing one there is replaced)
#   VERSION           the version the bundle reports, e.g. 0.4.0 (a leading v is dropped)
#   GUI_BIN, CLI_BIN  the vaultis-gui and vaultis executables -- normally universal
#                     binaries already joined with lipo (see .github/workflows/release.yml)
#   SAMPLE_VAULT_DIR  the seeded practice vault, or "" to ship without one
#
# The layout it produces:
#
#   vaultis.app/Contents/Info.plist
#   vaultis.app/Contents/MacOS/vaultis-gui          the app (CFBundleExecutable)
#   vaultis.app/Contents/MacOS/vaultis              the console binary: CLI and --tui
#   vaultis.app/Contents/Resources/vaultis.icns
#   vaultis.app/Contents/Resources/sample-vault/    copied out on first use, never opened
#                                                   in place (see launch::sample_vault_dir)
#
# Signing: with MACOS_SIGN_IDENTITY set (a "Developer ID Application: ..." identity in
# the keychain) the bundle is signed with the hardened runtime, ready for notarization.
# Without it the bundle is signed AD HOC, which is not a statement of who made it --
# it exists because Apple Silicon refuses to run code with no signature at all, and
# joining two architectures with lipo is not guaranteed to keep the per-slice ad hoc
# signatures the linker wrote.

set -euo pipefail

if [ "$#" -ne 5 ]; then
    sed -n '2,13p' "$0" >&2
    exit 2
fi

OUT_DIR=$1
VERSION=${2#v}
GUI_BIN=$3
CLI_BIN=$4
SAMPLE=$5

HERE=$(cd "$(dirname "$0")" && pwd)
ICON_PNG="$HERE/../icons/vaultis-locked.png"

APP="$OUT_DIR/vaultis.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

install -m 0755 "$GUI_BIN" "$APP/Contents/MacOS/vaultis-gui"
install -m 0755 "$CLI_BIN" "$APP/Contents/MacOS/vaultis"

# The bundle's version strings must be numeric (CFBundleVersion is compared
# component-wise by LaunchServices), so a prerelease suffix or a dev- label is cut off
# there and kept only in the human-readable string.
NUMERIC=$(printf '%s' "$VERSION" | sed -E 's/^([0-9]+(\.[0-9]+){0,2}).*/\1/')
case "$NUMERIC" in
    [0-9]*) ;;
    *) NUMERIC=0.0.0 ;;
esac

sed -e "s/@VERSION@/$VERSION/g" -e "s/@NUMERIC_VERSION@/$NUMERIC/g" \
    "$HERE/Info.plist.in" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist" >/dev/null

# The app icon: the locked vault, the same mark as the read-only default shortcut on the
# other platforms. iconutil wants every size in an .iconset; sips scales the one 512px
# master down (the 1024px @2x slot is left out rather than upscaled).
ICONSET=$(mktemp -d)/vaultis.iconset
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    if [ "$double" -le 512 ]; then
        sips -z "$double" "$double" "$ICON_PNG" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
    fi
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/vaultis.icns"
rm -rf "$(dirname "$ICONSET")"

if [ -n "$SAMPLE" ]; then
    if [ ! -f "$SAMPLE/vault.pmv" ]; then
        echo "error: $SAMPLE holds no vault.pmv" >&2
        exit 1
    fi
    # ditto rather than cp -R: it is the macOS-native copy and keeps the tree exactly.
    ditto "$SAMPLE" "$APP/Contents/Resources/sample-vault"
    # Session state, not vault content -- same reason release.yml drops it on Windows.
    rm -f "$APP/Contents/Resources/sample-vault/vaultis.lock"
fi

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    # Inside-out: the helper binary first, then the bundle (which seals the main
    # executable and every resource). --deep is deprecated for signing and not used.
    codesign --force --timestamp --options runtime --sign "$MACOS_SIGN_IDENTITY" \
        "$APP/Contents/MacOS/vaultis"
    codesign --force --timestamp --options runtime --sign "$MACOS_SIGN_IDENTITY" "$APP"
else
    codesign --force --sign - "$APP/Contents/MacOS/vaultis"
    codesign --force --sign - "$APP"
fi
codesign --verify --strict --verbose=2 "$APP"

echo "built $APP"
