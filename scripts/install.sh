#!/bin/sh
# Orchard installer for macOS and Linux.
#
#   curl -fsSL https://raw.githubusercontent.com/Artemis-Inc/Orchard/main/scripts/install.sh | sh
#
# Downloads the `orch` CLI for your platform from GitHub Releases and installs it
# to ~/.orchard/bin. Override with:
#   ORCHARD_VERSION=3.0.0   pin a version (default: latest release)
#   ORCHARD_INSTALL_DIR=... install location (default: $HOME/.orchard/bin)
set -eu

REPO="Artemis-Inc/Orchard"
INSTALL_DIR="${ORCHARD_INSTALL_DIR:-$HOME/.orchard/bin}"

say() { printf '\033[32m%s\033[0m\n' "orchard: $1"; }
err() { printf '\033[31m%s\033[0m\n' "orchard: $1" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || err "this installer needs '$1' but it was not found"; }
need uname
need tar

# Pick a downloader.
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fsSL "$1" -o "$2"; }
  fetch() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -qO "$2" "$1"; }
  fetch() { wget -qO- "$1"; }
else
  err "need curl or wget"
fi

# Detect target triple.
os="$(uname -s)"
arch="$(uname -m)"
case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      x86_64)        target="x86_64-apple-darwin" ;;
      *) err "unsupported macOS architecture: $arch" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64)        target="x86_64-unknown-linux-gnu" ;;
      aarch64|arm64) target="aarch64-unknown-linux-gnu" ;;
      *) err "unsupported Linux architecture: $arch" ;;
    esac ;;
  *) err "unsupported OS: $os (use the Windows installer on Windows)" ;;
esac

# Resolve the version.
version="${ORCHARD_VERSION:-}"
if [ -z "$version" ]; then
  say "resolving latest release"
  tag="$(fetch "https://api.github.com/repos/$REPO/releases/latest" \
    | grep -m1 '"tag_name"' | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')"
  [ -n "$tag" ] || err "could not determine the latest release"
  version="${tag#v}"
fi

asset="orch-${version}-${target}.tar.gz"
url="https://github.com/$REPO/releases/download/v${version}/${asset}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say "downloading $asset"
dl "$url" "$tmp/$asset" || err "download failed: $url"

# Verify the checksum if the release publishes one.
if fetch "https://github.com/$REPO/releases/download/v${version}/SHA256SUMS" > "$tmp/SHA256SUMS" 2>/dev/null \
   && grep -q "$asset" "$tmp/SHA256SUMS"; then
  if command -v shasum >/dev/null 2>&1; then sum="shasum -a 256"; else sum="sha256sum"; fi
  want="$(grep "$asset" "$tmp/SHA256SUMS" | awk '{print $1}')"
  got="$($sum "$tmp/$asset" | awk '{print $1}')"
  [ "$want" = "$got" ] || err "checksum mismatch for $asset"
  say "checksum verified"
fi

say "installing to $INSTALL_DIR"
tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$INSTALL_DIR"
mv "$tmp/orch" "$INSTALL_DIR/orch"
chmod +x "$INSTALL_DIR/orch"

say "installed orch ${version}"

# PATH hint.
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    printf '\n'
    say "add this to your shell profile, then restart your shell:"
    printf '    export PATH="%s:$PATH"\n\n' "$INSTALL_DIR"
    ;;
esac

"$INSTALL_DIR/orch" --version 2>/dev/null || true
say "done. run 'orch --help' to get started."
