#!/bin/sh
# Regenerate the Homebrew formula for a released version from its SHA256SUMS.
#
#   scripts/update-homebrew.sh <version> [sha256sums-file] [output.rb]
#
# <version>          release version without the leading v, e.g. 3.1.0
# [sha256sums-file]  path to the published SHA256SUMS (default: download it)
# [output.rb]        where to write the formula (default: packaging/homebrew/orchard.rb)
#
# The release workflow calls this after publishing assets, then pushes the
# result to the tap repo (Artemis-Inc/homebrew-orchard) so
# `brew install/upgrade artemis-inc/orchard/orchard` always tracks the latest.
set -eu

REPO="Artemis-Inc/Orchard"
version="${1:?usage: update-homebrew.sh <version> [sha256sums] [out.rb]}"
sums="${2:-}"
out="${3:-packaging/homebrew/orchard.rb}"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

if [ -z "$sums" ]; then
  sums="$tmp/SHA256SUMS"
  curl -fsSL "https://github.com/$REPO/releases/download/v${version}/SHA256SUMS" -o "$sums"
fi

# Pull each platform's checksum out of the SHA256SUMS file.
sha_for() {
  asset="orch-${version}-$1.tar.gz"
  line="$(grep " ${asset}\$" "$sums" || grep "${asset}" "$sums" || true)"
  sum="$(printf '%s\n' "$line" | awk '{print $1}')"
  [ -n "$sum" ] || { echo "missing checksum for $asset in $sums" >&2; exit 1; }
  printf '%s' "$sum"
}

mac_arm="$(sha_for aarch64-apple-darwin)"
mac_x64="$(sha_for x86_64-apple-darwin)"
lin_arm="$(sha_for aarch64-unknown-linux-gnu)"
lin_x64="$(sha_for x86_64-unknown-linux-gnu)"

url() { printf 'https://github.com/%s/releases/download/v%s/orch-%s-%s.tar.gz' "$REPO" "$version" "$version" "$1"; }

mkdir -p "$(dirname "$out")"
cat > "$out" <<EOF
# Homebrew formula for Orchard. This is the canonical copy; it is mirrored into
# the tap repository (Artemis-Inc/homebrew-orchard) by the release workflow so
# users can run:
#
#   brew install artemis-inc/orchard/orchard
#
# Do not hand-edit the version or sha256 values: the release workflow regenerates
# this file from the published SHA256SUMS on every tagged release
# (see .github/workflows/release.yml and scripts/update-homebrew.sh).
class Orchard < Formula
  desc "Typed, concurrent language for building LLM agents"
  homepage "https://github.com/Artemis-Inc/Orchard"
  version "${version}"
  license "MIT"

  # Lets \`brew livecheck\` (and Homebrew's autobump) detect new releases.
  livecheck do
    url "https://github.com/Artemis-Inc/Orchard/releases/latest"
    regex(%r{tag/v?(\d+(?:\.\d+)+)}i)
  end

  on_macos do
    on_arm do
      url "$(url aarch64-apple-darwin)"
      sha256 "${mac_arm}"
    end
    on_intel do
      url "$(url x86_64-apple-darwin)"
      sha256 "${mac_x64}"
    end
  end

  on_linux do
    on_arm do
      url "$(url aarch64-unknown-linux-gnu)"
      sha256 "${lin_arm}"
    end
    on_intel do
      url "$(url x86_64-unknown-linux-gnu)"
      sha256 "${lin_x64}"
    end
  end

  def install
    bin.install "orch"
  end

  test do
    assert_match "orch", shell_output("#{bin}/orch --version")
  end
end
EOF

echo "wrote $out for v${version}"
