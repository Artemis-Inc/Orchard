# Homebrew formula for Orchard. This is the canonical copy; it is mirrored into
# the tap repository (Artemis-Inc/homebrew-orchard) so users can run:
#
#   brew install artemis-inc/orchard/orchard
#
# The sha256 values are filled in when a release is built. Until a platform's
# binary is published, its block points at the release asset and its checksum is
# updated by the release process.
class Orchard < Formula
  desc "Typed, concurrent language for building LLM agents"
  homepage "https://github.com/Artemis-Inc/Orchard"
  version "3.0.0"
  license "Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.0.0/orch-3.0.0-aarch64-apple-darwin.tar.gz"
      sha256 "d51fb5dbdcb8128c25042bb29920a4ce869eec6e5a8591a264118e47b7372512"
    end
    on_intel do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.0.0/orch-3.0.0-x86_64-apple-darwin.tar.gz"
      sha256 "4ca2ed6a1563d96f129ac3c9eac16284e1d062f3e8dc2955cdab29680bfa4a2c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.0.0/orch-3.0.0-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_LINUX_ARM64_SHA256"
    end
    on_intel do
      url "https://github.com/Artemis-Inc/Orchard/releases/download/v3.0.0/orch-3.0.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "REPLACE_WITH_LINUX_X86_64_SHA256"
    end
  end

  def install
    bin.install "orch"
  end

  test do
    assert_match "orch", shell_output("#{bin}/orch --version")
  end
end
