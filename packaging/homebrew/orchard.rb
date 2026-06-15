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
  version "3.1.0"
  license "Apache-2.0"

  # Lets `brew livecheck` (and Homebrew's autobump) detect new releases.
  livecheck do
    url "https://github.com/Artemis-Inc/Orchard/releases/latest"
    regex(%r{tag/v?(\d+(?:\.\d+)+)}i)
  end

  on_macos do
    on_arm do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "351269375ac3ea3b8642e2df651261b04d0befe0a1762837fb77d75555305c61"
    end
    on_intel do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-x86_64-apple-darwin.tar.gz"
      sha256 "65fdffb0d736918db1c397cfffbebf5e2805c5792de6d0c0753fe2df52497280"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "9c459c9262cded001aef20af2f040a57fea0009c4999c3c484af2070404834c3"
    end
    on_intel do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.1.0/orch-3.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "7f6467068ff8304e120f2400bf22d0783058a89938ec5d9aa89d3a86cae0a7e5"
    end
  end

  def install
    bin.install "orch"
  end

  test do
    assert_match "orch", shell_output("#{bin}/orch --version")
  end
end
