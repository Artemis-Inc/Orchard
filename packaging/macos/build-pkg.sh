#!/usr/bin/env bash
# Build a macOS installer package that puts `orch` in /usr/local/bin.
#
#   packaging/macos/build-pkg.sh <version> <path-to-orch-binary> <output.pkg>
#
# The package is unsigned. To sign and notarize, set:
#   PKG_SIGN_ID  (Developer ID Installer: ... certificate in your keychain)
# and the script will sign with productsign.
set -euo pipefail

VERSION="${1:?usage: build-pkg.sh <version> <binary> <output.pkg>}"
BIN="${2:?missing binary path}"
OUT="${3:?missing output path}"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

mkdir -p "$STAGE/root/usr/local/bin"
cp "$BIN" "$STAGE/root/usr/local/bin/orch"
chmod 755 "$STAGE/root/usr/local/bin/orch"

pkgbuild \
  --root "$STAGE/root" \
  --identifier "inc.artemis.orchard" \
  --version "$VERSION" \
  --install-location "/" \
  "$STAGE/orchard-component.pkg"

productbuild \
  --identifier "inc.artemis.orchard.installer" \
  --version "$VERSION" \
  --package "$STAGE/orchard-component.pkg" \
  "$STAGE/unsigned.pkg"

if [ -n "${PKG_SIGN_ID:-}" ]; then
  productsign --sign "$PKG_SIGN_ID" "$STAGE/unsigned.pkg" "$OUT"
  echo "signed $OUT"
else
  cp "$STAGE/unsigned.pkg" "$OUT"
  echo "built $OUT (unsigned)"
fi
